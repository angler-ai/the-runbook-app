import { randomUUID } from 'node:crypto';
import type pg from 'pg';
import { withTransaction } from '@runbook/runner-shared/db';
import type { ReleaseMaterial } from '@runbook/runner-shared/contracts/internal';
import { audit } from '../audit.ts';
import { VaultError } from '../errors.ts';
import { OAuthConnection, warningMs, withRefreshLock } from './connection.ts';
import { formRequest, ProviderRequestError, tokenPair } from './provider.ts';
import { destroyFields, getConnection, inTransaction, locked, oauthCredential, random, retryRefresh, unavailable, writeSecret } from './state.ts';

const day = 86_400_000;
const hour = 3_600_000;
const cooldown = 30_000;
const rejectionWindow = 300_000;
const refreshBackoff = (attempt: number) => cooldown * 2 ** Math.min(attempt, 6);
async function enqueueAlert(db: pg.PoolClient, id: string, kind: 'credential.reconnect_required' | 'credential.oauth_expiring' | 'credential.oauth_refresh_failing',
  now: Date, details: { reason: string } | { expires_at_ms: number } | { since_ms: number }): Promise<void> {
  const eventId = randomUUID();
  await db.query("INSERT INTO budget_event_outbox(id, group_id, runner_id, kind, data) VALUES ($1, '', '', $2, $3)",
    [eventId, kind, JSON.stringify({ event_id: eventId, credential_id: id, occurred_at_ms: now.getTime(), ...details })]);
}

const worker = { kind: 'process', id: 'vault' } as const;

export type RefreshReason = 'release' | 'test' | 'renewal' | 'keep-alive';
export interface OAuthAccess {
  material: ReleaseMaterial;
  binding: Record<string, unknown>;
  expiresAt: number;
  version: number;
  generation: number;
}

export class OAuthAccessService extends OAuthConnection {
  private maintenance: Promise<void> | undefined;
  private readonly inFlight = new Map<string, Promise<void>>();

  async ensureAccess(id: string, reason: RefreshReason, observedVersion?: number): Promise<void> {
    const pending = this.inFlight.get(id);
    if (pending) {
      try {
        await pending;
        if (observedVersion === undefined) return;
      } catch (error) {
        // A concurrent forced refresh can fail while ordinary callers can
        // still use the current token. Recheck its expiry under the lock.
        if (observedVersion !== undefined || !(error instanceof VaultError) || error.code !== 'oauth.refresh_retry') throw error;
      }
    }
    const operation = withRefreshLock(this.d.pool, id, (db) => this.refresh(db, id, reason, observedVersion));
    this.inFlight.set(id, operation);
    try {
      await operation;
    } finally {
      if (this.inFlight.get(id) === operation) this.inFlight.delete(id);
    }
  }

  private async refresh(db: pg.PoolClient, id: string, reason: RefreshReason, observedVersion?: number): Promise<void> {
    const { d } = this;
    const operation = random();
    const prepared = await inTransaction(db, async () => {
      await locked(db, id);
      const credential = await oauthCredential(d.secrets, db, id);
      const binding = d.secrets.binding(credential);
      const row = await getConnection(db, id);
      if (!row?.access_expires_at || credential.status !== 'active') return { blocked: true } as const;
      const now = d.now();
      const nowMs = now.getTime();
      const accessExpiry = row.access_expires_at.getTime();
      if (!row.connection_hash?.equals(binding.hash) || row.refresh_operation ||
          (row.grant_expires_at && row.grant_expires_at <= now) ||
          (!row.refresh_version && accessExpiry <= nowMs)) {
        await this.block(db, id, row.generation, 'expiry_or_interrupted_refresh');
        return { blocked: true } as const;
      }
      if (!row.refresh_version) {
        let first = row.access_rejected_since?.getTime();
        if (row.access_version && first !== undefined) {
          const lastUsed = nowMs - first <= rejectionWindow
            ? await d.gateway.lastOAuthUse(id, row.generation, row.access_version).catch(() => undefined)
            : undefined;
          if (nowMs - first > rejectionWindow || (lastUsed !== undefined && lastUsed > first && lastUsed <= nowMs)) {
            await db.query('UPDATE credential_oauth SET access_rejected_since = NULL WHERE credential_id = $1', [id]);
            first = undefined;
          }
        }
        if (observedVersion === row.access_version) {
          if (first !== undefined && nowMs - first >= cooldown) {
            await this.block(db, id, row.generation, 'access_only_rejected');
            return { blocked: true } as const;
          }
          if (first === undefined) {
            await db.query('UPDATE credential_oauth SET access_rejected_since = $2 WHERE credential_id = $1', [id, now]);
          }
        }
        return;
      }
      const forced = observedVersion !== undefined && observedVersion === row.access_version;
      if (row.refresh_failure_since) {
        const failedFor = nowMs - row.refresh_failure_since.getTime();
        if (failedFor >= day) {
          await this.block(db, id, row.generation, 'refresh_failure_timeout');
          return { blocked: true } as const;
        }
        if (failedFor >= hour && !row.refresh_failure_warned_at) {
          await enqueueAlert(db, id, 'credential.oauth_refresh_failing', now, { since_ms: row.refresh_failure_since.getTime() });
          await db.query('UPDATE credential_oauth SET refresh_failure_warned_at = $2 WHERE credential_id = $1', [id, now]);
        }
      }
      const half = ((row.access_issued_at?.getTime() ?? nowMs) + accessExpiry) / 2;
      const due = nowMs >= Math.min(half, accessExpiry - 300_000);
      const keepAlive = reason === 'keep-alive' && nowMs - (row.refreshed_at?.getTime() ?? 0) >= day;
      if (!row.refresh_failures && (reason === 'keep-alive' ? !keepAlive : !(forced || due))) return;
      if (row.refresh_after && row.refresh_after > now) {
        return accessExpiry <= nowMs ? { retry: true } as const : undefined;
      }
      const config = credential.config;
      if (typeof config.token_endpoint !== 'string' || typeof config.client_id !== 'string' || typeof config.resource !== 'string') {
        await this.block(db, id, row.generation, 'invalid_binding');
        return { blocked: true } as const;
      }
      const opened = await d.secrets.openForRelease({ credentialId: id, field: 'refresh_token', pinnedVersion: row.refresh_version }, db);
      const refreshToken = opened.value.toString('utf8');
      let request;
      let clientSecret: string | undefined;
      try {
        clientSecret = await this.clientSecret(db, credential, row);
        request = formRequest(config,
          new URLSearchParams({ grant_type: 'refresh_token', refresh_token: refreshToken,
            client_id: config.client_id, resource: config.resource }), clientSecret);
      } catch {
        await this.block(db, id, row.generation, 'client_authentication_unavailable');
        return { blocked: true } as const;
      } finally {
        opened.value.fill(0);
      }
      const forcedCount = forced ? Math.min(row.forced_refreshes + 1, 7) : 0;
      await db.query('UPDATE credential_oauth SET refresh_operation = $2, refresh_after = $3, forced_refreshes = $4 WHERE credential_id = $1',
        [id, operation, new Date(nowMs + refreshBackoff(Math.max(0, forcedCount - 1))), forcedCount]);
      await audit(db, { actor: worker, action: 'credential.oauth_refresh_started', resource_type: 'credential',
        resource_id: id, group_id: null, runner_id: null, detail: { reason, token_version: row.access_version } });
      return { generation: row.generation, version: row.access_version, refreshVersion: row.refresh_version,
        accessExpiry, forced, hash: binding.hash, url: config.token_endpoint, request, refreshToken, clientSecret, scopes: credential.oauth_status?.granted_scopes ?? (binding.binding.scopes as string[]) } as const;
    });
    if (!prepared) return;
    if ('blocked' in prepared) throw unavailable();
    if ('retry' in prepared) throw retryRefresh();
    let pair;
    try {
      pair = tokenPair(await d.provider.request(prepared.url, prepared.request), d.now(), prepared.scopes, prepared.clientSecret, prepared.refreshToken, true);
    } catch (error) {
      const retryable = error instanceof VaultError && (error.code === 'oauth.refresh_retry'
        || (error instanceof ProviderRequestError && !error.sent));
      const current = await inTransaction(db, async () => {
        await locked(db, id);
        const row = await getConnection(db, id);
        if (row?.generation === prepared.generation && row.refresh_operation === operation) {
          if (retryable) {
            const failures = row.refresh_failures + 1;
            const failureClass = error instanceof ProviderRequestError ? 'transport_unavailable'
              : error.status === 503 ? 'provider_unavailable' : 'provider_oauth_error';
            await db.query(`UPDATE credential_oauth SET refresh_operation = NULL, refresh_failures = $2,
              refresh_failure_since = COALESCE(refresh_failure_since, $4),
              refresh_after = GREATEST(refresh_after, $3) WHERE credential_id = $1`,
            [id, failures, new Date(d.now().getTime() + refreshBackoff(failures - 1)), d.now()]);
            await db.query('UPDATE credentials SET oauth_status = oauth_status || $2::jsonb WHERE id = $1',
              [id, JSON.stringify({ refresh_failure_since: row.refresh_failure_since ?? d.now(),
                refresh_failures: failures, last_refresh_error: failureClass })]);
          } else {
            const code = error instanceof VaultError ? error.code : '';
            const reason = ['oauth.invalid_grant', 'oauth.invalid_client', 'oauth.unauthorized_client'].includes(code)
              ? code.slice('oauth.'.length) : 'ambiguous_refresh';
            await this.block(db, id, row.generation, reason);
          }
          return true;
        }
        return false;
      });
      if (!current || !retryable) throw unavailable();
      if (prepared.forced || prepared.accessExpiry <= d.now().getTime()) throw retryRefresh();
      return;
    }
    const committed = await inTransaction(db, async () => {
      await locked(db, id);
      const row = await getConnection(db, id);
      const credential = await oauthCredential(d.secrets, db, id);
      if (row?.generation !== prepared.generation || row.refresh_operation !== operation || row.access_version !== prepared.version) return false;
      if (!prepared.hash.equals(d.secrets.binding(credential).hash)) {
        await this.block(db, id, row.generation, 'binding_changed');
        return false;
      }
      const now = d.now();
      await destroyFields(db, id, pair.refresh ? ['access_token', 'refresh_token'] : ['access_token'], now);
      const access = await writeSecret(d.secrets, db, credential, 'access_token', pair.access, worker.id);
      const refresh = pair.refresh ? await writeSecret(d.secrets, db, credential, 'refresh_token', pair.refresh, worker.id) : prepared.refreshVersion;
      await db.query(`UPDATE credential_oauth SET access_version = $2, refresh_version = $3, access_issued_at = $4,
        access_expires_at = $5, refreshed_at = $4, refresh_operation = NULL, refresh_failures = 0,
        refresh_failure_since = NULL, refresh_failure_warned_at = NULL WHERE credential_id = $1`,
      [id, access, refresh, now, pair.expiresAt]);
      // Keep the account display approved at confirmation.
      await db.query(`UPDATE credentials SET oauth_status = (oauth_status - 'refresh_failure_since'
        - 'refresh_failures' - 'last_refresh_error') || $2::jsonb WHERE id = $1`,
        [id, JSON.stringify({ access_expires_at: pair.expiresAt, granted_scopes: pair.scopes })]);
      await audit(db, { actor: worker, action: 'credential.oauth_refreshed', resource_type: 'credential',
        resource_id: id, group_id: null, runner_id: null, detail: { token_version: access } });
      return true;
    });
    if (!committed) throw unavailable();
  }

  /** Called under the credential lock before replacing a confirmed binding. */
  async changeBinding(db: pg.PoolClient, id: string, next: { config: Record<string, unknown>; groups: unknown }, actor: string): Promise<boolean> {
    const { d } = this;
    const credential = await oauthCredential(d.secrets, db, id);
    const row = await getConnection(db, id);
    if (!row || !credential.config.issuer) return false;
    if (credential.config.registration_mode === 'dynamic' && credential.config.token_endpoint_auth_method !== 'none') {
      // The registration endpoint and client identity are immutable. Rebind
      // the registration secret to permitted scope/group edits atomically.
      const opened = await d.secrets.openForRelease({ credentialId: id, field: 'client_secret' }, db);
      try {
        await writeSecret(d.secrets, db, { ...credential, ...next }, 'client_secret', opened.value.toString('utf8'), actor);
      } finally {
        opened.value.fill(0);
      }
    }
    if (!row.connection_hash) return false;
    const generation = row.generation + 1;
    await d.gateway.fenceOAuth(id, generation);
    await db.query(`UPDATE credential_oauth SET generation = $2, refresh_operation = NULL,
      pending_state = NULL, pending_claim = NULL WHERE credential_id = $1`, [id, generation]);
    await enqueueAlert(db, id, 'credential.reconnect_required', d.now(), { reason: 'binding_changed' });
    return true;
  }

  private async block(db: pg.PoolClient, id: string, generation: number, reason: string): Promise<void> {
    const now = this.d.now();
    await db.query(`UPDATE credentials SET status = 'reconnect_required', updated_at = $2,
      oauth_status = COALESCE(oauth_status, '{}') || $3::jsonb WHERE id = $1`,
      [id, now, JSON.stringify({ connect_result: 'reconnect_required', reconnect_reason: reason })]);
    await db.query('UPDATE credential_oauth SET generation = $2, refresh_operation = NULL, pending_claim = NULL, pending_state = NULL WHERE credential_id = $1',
      [id, generation + 1]);
    await this.d.gateway.fenceOAuth(id, generation + 1).catch(() => undefined);
    await enqueueAlert(db, id, 'credential.reconnect_required', now, { reason });
    await audit(db, { actor: worker, action: 'credential.reconnect_required', resource_type: 'credential', resource_id: id,
      group_id: null, runner_id: null, detail: { reason } });
  }

  async recordSuccessfulAccess(id: string, version: number, startedAt: Date): Promise<void> {
    await this.d.pool.query(`UPDATE credential_oauth SET access_rejected_since = NULL
      WHERE credential_id = $1 AND access_version = $2 AND access_rejected_since <= $3`, [id, version, startedAt]);
  }

  /** Serialize the last binding check and the material push with disconnect/reconnect. */
  async withAccess<T>(id: string, grantExpiry: number, action: (access: OAuthAccess, db: pg.PoolClient) => Promise<T>): Promise<T> {
    const { d } = this;
    return withTransaction(d.pool, async (db) => {
      await locked(db, id);
      const credential = await oauthCredential(d.secrets, db, id);
      const row = await getConnection(db, id);
      if (credential.status !== 'active' || !row?.access_version || !row.access_expires_at ||
          !row.connection_hash?.equals(d.secrets.binding(credential).hash)) throw unavailable();
      const expiresAt = Math.min(grantExpiry, row.access_expires_at.getTime(), row.grant_expires_at?.getTime() ?? Infinity);
      if (expiresAt <= d.now().getTime()) throw unavailable();
      const opened = await d.secrets.openForRelease({ credentialId: id, field: 'access_token', pinnedVersion: row.access_version }, db);
      try {
        return await action({
          material: { fields: {}, mcp_oauth: { access_token: opened.value.toString('utf8'),
            generation: row.generation, token_version: row.access_version } },
          binding: opened.binding, expiresAt, version: row.access_version, generation: row.generation,
        }, db);
      } finally {
        opened.value.fill(0);
      }
    });
  }

  maintain(): Promise<void> {
    this.maintenance ??= this.maintenancePass().finally(() => { this.maintenance = undefined; });
    return this.maintenance;
  }

  private async maintenancePass(): Promise<void> {
    const { d } = this;
    await this.retryPurges();
    await this.discardExpiredAuthorizations();
    const cutoff = new Date(d.now().getTime() + warningMs);
    const stale = new Date(d.now().getTime() - day);
    const { rows } = await d.pool.query<{ credential_id: string }>(
      `SELECT o.credential_id FROM credential_oauth o JOIN credentials c ON c.id = o.credential_id
       WHERE c.status = 'active' AND (o.refresh_failures > 0 OR o.refresh_operation IS NOT NULL OR o.grant_expires_at <= $1 OR
         (o.refresh_version IS NULL AND (o.access_expires_at <= $1 OR o.access_rejected_since IS NOT NULL)) OR
         (o.refresh_version IS NOT NULL AND o.refreshed_at <= $2) OR (o.grant_expires_at <= $3 AND o.warned_at IS NULL))
       ORDER BY o.refreshed_at LIMIT 100`, [d.now(), stale, cutoff]);
    for (const { credential_id: id } of rows) {
      await this.ensureAccess(id, 'keep-alive').catch(() => undefined);
      await withTransaction(d.pool, async (db) => {
        await locked(db, id);
        const row = await getConnection(db, id);
        const credential = await oauthCredential(d.secrets, db, id);
        if (credential.status !== 'active' || !row?.grant_expires_at || row.grant_expires_at > cutoff || row.warned_at) return;
        await db.query('UPDATE credential_oauth SET warned_at = $2 WHERE credential_id = $1', [id, d.now()]);
        await db.query(`UPDATE credentials SET oauth_status = oauth_status || '{"expiry_warning":true}'::jsonb WHERE id = $1`, [id]);
        await enqueueAlert(db, id, 'credential.oauth_expiring', d.now(), { expires_at_ms: row.grant_expires_at.getTime() });
        await audit(db, { actor: worker, action: 'credential.oauth_expiring', resource_type: 'credential', resource_id: id,
          group_id: null, runner_id: null, detail: { expires_at: row.grant_expires_at.toISOString() } });
      }).catch(() => undefined);
    }
    await d.pool.query(`UPDATE credential_secret_versions v SET wrapped_dek = NULL, destroyed_at = $1
      FROM credential_oauth o WHERE v.credential_id = o.credential_id
        AND v.destroyed_at IS NULL AND v.field IN ('access_token', 'refresh_token')
        AND o.connection_hash IS NULL`, [d.now()]);
  }
}
