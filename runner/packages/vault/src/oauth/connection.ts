import type pg from 'pg';
import type { Logger } from '@runbook/runner-shared/log';
import { withTransaction } from '@runbook/runner-shared/db';

import type { OAuthDiscoveryCandidate } from '@runbook/runner-shared/credential-source';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { oauthAuthorizationUrl, oauthUrl } from '@runbook/runner-shared/oauth-binding';
import { audit } from '../audit.ts';
import { VaultError } from '../errors.ts';
import type { SecretStore } from '../secrets.ts';
import type { VaultIdentity } from '../identity.ts';
import type { GatewayClient } from '../gateway.ts';
import { record } from '../types/common.ts';
import { discoverOAuth, formRequest, ProviderRequestError, registerOAuth, tokenPair, type OAuthProvider } from './provider.ts';
import {
  conflict, destroyFields, digest, digestPattern, getConnection, inTransaction, invalidCallback,
  locked, mismatch, oauthCredential, random, requireAdmin, sessionHash, unavailable, writeSecret,
  type ConnectionRow, type OAuthCredential,
} from './state.ts';

const day = 86_400_000;
export const warningMs = 14 * day;

export interface OAuthDeps {
  pool: pg.Pool;
  secrets: SecretStore;
  gateway: GatewayClient;
  provider: OAuthProvider;
  identity: VaultIdentity;
  installId?: string;
  now(): Date;
  log: Logger;
}

function requirePin(value: unknown, hash: Buffer) {
  if (typeof value !== 'string' || value !== hash.toString('base64url')) throw mismatch();
}

function configured(config: Record<string, unknown>): asserts config is Record<string, unknown> & {
  url: string; resource: string; issuer: string; redirect_uri: string; client_id: string;
  authorization_endpoint: string; token_endpoint: string;
} {
  if (!config.client_id || !config.issuer || !config.redirect_uri) throw unavailable();
}

export async function withRefreshLock<T>(pool: pg.Pool, id: string, operation: (client: pg.PoolClient) => Promise<T>): Promise<T> {
  // In addition to the durable operation marker, this session lock covers the
  // entire network call. It has no lease that could expire in flight.
  const client = await pool.connect();
  let destroy = false;
  try {
    await client.query('SELECT pg_advisory_lock(hashtextextended($1, 7419028))', [id]);
    return await operation(client);
  } finally {
    try {
      await client.query('SELECT pg_advisory_unlock(hashtextextended($1, 7419028))', [id]);
    } catch {
      destroy = true;
    }
    client.release(destroy);
  }
}

export class OAuthConnection {
  protected readonly d: OAuthDeps;

  constructor(deps: OAuthDeps) {
    this.d = deps;
  }

  async restoreDiscoveryCandidates(orgId: string): Promise<void> {
    // Older vaults saved the candidate privately without exposing it in the
    // credential read model. Restore only candidates for the current draft.
    await this.d.pool.query(`UPDATE credentials c SET oauth_status = COALESCE(c.oauth_status, '{}') ||
      jsonb_build_object('discovery_candidate', jsonb_strip_nulls(jsonb_build_object(
        'config', o.candidate, 'groups', c.groups, 'confirmed', false,
        'registration_pending', CASE WHEN o.registration_operation IS NOT NULL THEN true END)))
      FROM credential_oauth o WHERE c.id = o.credential_id AND c.org_id = $1 AND c.type = 'mcp-oauth'
        AND c.deleted_at IS NULL AND c.status <> 'revoked' AND c.config->>'issuer' IS NULL
        AND c.oauth_status->'discovery_candidate' IS NULL AND o.candidate IS NOT NULL AND o.candidate_hash = c.binding_hash`,
    [orgId]);
  }

  async discover(id: string, claims: UserAssertionClaims) {
    requireAdmin(claims);
    const { d } = this;
    const credential = await oauthCredential(d.secrets, d.pool, id);
    if (credential.config.issuer) return { config: credential.config, confirmed: true };
    const stored = await getConnection(d.pool, id);
    if (stored?.registration_operation && stored.candidate && stored.candidate_hash?.equals(d.secrets.binding(credential).hash)) {
      return { config: stored.candidate, groups: credential.groups, confirmed: false, registration_pending: true };
    }
    const { hash } = d.secrets.binding(credential);
    const origin = new URL(oauthUrl(claims.iss, 'core origin', false)).origin;
    const config = { ...await discoverOAuth(d.provider, credential.config),
      redirect_uri: `${origin}/v1/runner/oauth/callback/${encodeURIComponent(id)}` };
    const { groups } = d.secrets.binding({ ...credential, config });
    const candidate: OAuthDiscoveryCandidate = { config, groups, confirmed: false };
    await withTransaction(d.pool, async (db) => {
      await locked(db, id);
      const current = await oauthCredential(d.secrets, db, id);
      if (current.config.issuer || !d.secrets.binding(current).hash.equals(hash)) throw conflict();
      const saved = await db.query(`INSERT INTO credential_oauth(credential_id, candidate, candidate_hash) VALUES ($1,$2,$3)
        ON CONFLICT(credential_id) DO UPDATE SET candidate = $2, candidate_hash = $3
        WHERE credential_oauth.registration_operation IS NULL`, [id, JSON.stringify(config), hash]);
      if (!saved.rowCount) throw conflict();
      await db.query(`UPDATE credentials SET oauth_status = COALESCE(oauth_status, '{}') || $2::jsonb WHERE id = $1`,
        [id, JSON.stringify({ discovery_candidate: candidate })]);
    });
    return candidate;
  }

  async confirm(id: string, claims: UserAssertionClaims, body: unknown) {
    requireAdmin(claims);
    return withRefreshLock(this.d.pool, id, (db) => this.confirmLocked(db, id, claims, body));
  }

  private async confirmLocked(db: pg.PoolClient, id: string, claims: UserAssertionClaims, body: unknown) {
    const { d } = this;
    const { binding_hash: pin, reset_registration: reset, confirm_account: confirmation, authorization_handoff: handoff } = record(body);
    if (handoff !== undefined) return this.readAuthorization(db, id, claims, pin, handoff);
    if (confirmation !== undefined) return this.confirmAccount(db, id, claims, pin, confirmation);
    const operation = random();
    const claimed = await inTransaction(db, async () => {
      await locked(db, id);
      const credential = await oauthCredential(d.secrets, db, id);
      if (credential.config.issuer) return { config: credential.config, credential, registered: true };
      const row = await getConnection(db, id);
      if (!row?.candidate || !row.candidate_hash?.equals(d.secrets.binding(credential).hash) || (row.registration_operation && reset !== true)) throw conflict();
      const config = row.candidate;
      requirePin(pin, d.secrets.binding({ ...credential, config }).hash);
      await db.query('UPDATE credential_oauth SET registration_operation = $2 WHERE credential_id = $1', [id, operation]);
      await db.query(`UPDATE credentials SET oauth_status = COALESCE(oauth_status, '{}') || $2::jsonb WHERE id = $1`,
        [id, JSON.stringify({ discovery_candidate: { config, groups: credential.groups, confirmed: false, registration_pending: true } })]);
      return { config, credential, registered: false };
    });
    if (claimed.registered) return { config: claimed.config, groups: claimed.credential.groups, confirmed: true };
    const config = claimed.config;
    const registration = config.registration_mode === 'dynamic' ? await registerOAuth(d.provider, config).catch(async (error: unknown) => {
      if (error instanceof VaultError && (error.code === 'oauth.registration_refused'
          || (error instanceof ProviderRequestError && !error.sent))) {
        await inTransaction(db, async () => {
          await locked(db, id);
          const cleared = await db.query('UPDATE credential_oauth SET registration_operation = NULL WHERE credential_id = $1 AND registration_operation = $2', [id, operation]);
          if (cleared.rowCount) await db.query(`UPDATE credentials SET oauth_status = oauth_status #- '{discovery_candidate,registration_pending}' WHERE id = $1`, [id]);
        });
      }
      throw error;
    }) : undefined;
    if (registration) config.client_id = registration.clientId;
    return inTransaction(db, async () => {
      await locked(db, id);
      const credential = await oauthCredential(d.secrets, db, id);
      const row = await getConnection(db, id);
      if (row?.registration_operation !== operation || !row.candidate_hash?.equals(d.secrets.binding(credential).hash)) throw conflict();
      const updated = { ...credential, config };
      const binding = d.secrets.binding(updated);
      await db.query(`UPDATE credentials SET config = $2, binding = $3, binding_hash = $4, updated_at = $5,
        oauth_status = NULLIF(oauth_status - 'discovery_candidate', '{}'::jsonb) WHERE id = $1`,
        [id, JSON.stringify(config), JSON.stringify(binding.binding), binding.hash, d.now()]);
      if (registration?.secret) await writeSecret(d.secrets, db, updated, 'client_secret', registration.secret, claims.user_id);
      await db.query(`UPDATE credential_oauth SET candidate = NULL, candidate_hash = NULL, registration_operation = NULL,
        client_secret_expires_at = $2 WHERE credential_id = $1`, [id, registration?.secretExpiresAt ?? null]);
      return { config, groups: binding.groups, confirmed: true };
    });
  }

  async start(id: string, claims: UserAssertionClaims, body: unknown) {
    const session = sessionHash(claims);
    if (claims.fresh_mfa !== true) throw new VaultError(403, 'auth.mfa_step_up', 'OAuth connect requires a fresh second factor');
    const { d } = this;
    const { binding_hash: pin, authorization_handoff: handoff = '' } = record(body);
    if (typeof handoff !== 'string' || (handoff && !digestPattern.test(handoff))) throw conflict();
    return withRefreshLock(d.pool, id, async (db) => {
      const { authorization, discarded } = await inTransaction(db, async () => {
        await locked(db, id);
        const credential = await oauthCredential(d.secrets, db, id);
        const config = credential.config;
        configured(config);
        const expectedRedirect = `${new URL(claims.iss).origin}/v1/runner/oauth/callback/${encodeURIComponent(id)}`;
        if (config.redirect_uri !== expectedRedirect) throw mismatch();
        const row = await getConnection(db, id);
        if (!row || row.purge_until) throw conflict();
        const { hash, binding } = d.secrets.binding(credential);
        requirePin(pin, hash);
        await this.clientSecret(db, credential, row);
        const state = random();
        const verifier = random();
        const nonce = random();
        const now = d.now();
        const authorization_url = oauthAuthorizationUrl(binding, state, digest(verifier).toString('base64url'));
        const expires_at_ms = now.getTime() + 600_000;
        const nonce_hash = digest(nonce).toString('base64url');
        const request_id = claims.jti;
        const identity_signature = d.identity.oauthStart([
          claims.org_id, d.installId ?? '', id, hash.toString('base64url'), authorization_url,
          nonce_hash, session, request_id, String(expires_at_ms), handoff,
        ]);
        const authorization = { authorization_url, expires_at_ms, identity_signature, request_id,
          nonce_hash, session_hash: session };
        const discarded = await this.pendingRevocation(db, credential, row);
        await destroyFields(db, id, ['oauth_pending'], now);
        const version = await writeSecret(d.secrets, db, credential, 'oauth_pending', JSON.stringify({
          state, verifier, nonce, user: claims.user_id, session, generation: row.generation,
          hash: hash.toString('base64url'), expires: expires_at_ms,
          ...(handoff && { handoff, authorization }),
        }), claims.user_id);
        await db.query(`UPDATE credential_oauth SET pending_state = $2, pending_user = $3, pending_session = $4, pending_nonce = $5,
          pending_hash = $6, pending_version = $7, pending_expires_at = $8, pending_claim = NULL WHERE credential_id = $1`,
        [id, digest(state), claims.user_id, session, digest(nonce), hash, version, new Date(now.getTime() + 600_000)]);
        await db.query(`UPDATE credentials SET oauth_status = COALESCE(oauth_status, '{}') || $2::jsonb WHERE id = $1`,
          [id, JSON.stringify({ connect_result: 'pending', connect_test: null, connect_attempt_at: now, pending_account: null, account_confirmation: null, account_confirmation_signature: null, account_confirmation_expires_at_ms: null, account_changed: false })]);
        return { authorization: { ...authorization, nonce }, discarded };
      });
      await this.revokeGrant(discarded);
      return authorization;
    });
  }

  private async readAuthorization(db: pg.PoolClient, id: string, claims: UserAssertionClaims, pin: unknown, handoff: unknown) {
    if (typeof handoff !== 'string' || !digestPattern.test(handoff)) throw conflict();
    const { d } = this;
    return inTransaction(db, async () => {
      await locked(db, id);
      const credential = await oauthCredential(d.secrets, db, id);
      const { hash } = d.secrets.binding(credential);
      requirePin(pin, hash);
      const row = await getConnection(db, id);
      if (!row?.pending_version || row.pending_claim || row.pending_user !== claims.user_id ||
          !row.pending_hash?.equals(hash) || !row.pending_expires_at || row.pending_expires_at <= d.now()) {
        return {};
      }
      const opened = await d.secrets.openForRelease({ credentialId: id, field: 'oauth_pending', pinnedVersion: row.pending_version }, db);
      try {
        const sealed = record(JSON.parse(opened.value.toString('utf8')) as unknown);
        return sealed.handoff === handoff ? { authorization: sealed.authorization } : {};
      } finally {
        opened.value.fill(0);
      }
    });
  }

  async callback(id: string, claims: UserAssertionClaims, body: unknown) {
    const diagnostic: { phase: 'callback' | 'exchange'; providerStatus?: number } = { phase: 'callback' };
    const failed = async (errorClass: string) => {
      this.d.log.warn('OAuth connect failed', { error_class: errorClass,
        ...(diagnostic.providerStatus !== undefined && { provider_status: diagnostic.providerStatus }) });
      await audit(this.d.pool, {
        actor: { kind: 'user', id: claims.user_id }, action: `credential.oauth_${diagnostic.phase}_failed`,
        resource_type: 'credential', resource_id: id, runner_id: null, group_id: null,
        detail: { error_class: errorClass },
      });
    };
    let result;
    try {
      sessionHash(claims);
      result = await withRefreshLock(this.d.pool, id, (db) => this.exchange(db, id, claims, body, diagnostic));
    } catch (error) {
      if (error instanceof ProviderRequestError) diagnostic.providerStatus = error.providerStatus;
      await failed(error instanceof VaultError ? error.code : 'internal');
      throw error;
    }
    if (result.failure !== undefined) {
      await failed(result.failure);
      return { connected: false };
    }
    return result;
  }

  private async exchange(db: pg.PoolClient, id: string, claims: UserAssertionClaims, body: unknown,
    diagnostic: { phase: 'callback' | 'exchange'; providerStatus?: number }) {
    const session = sessionHash(claims);
    const { d } = this;
    const callback = record(body);
    const { code, state, nonce, iss, error } = callback;
    if (typeof state !== 'string' || !digestPattern.test(state) || typeof nonce !== 'string' || !digestPattern.test(nonce) ||
        (code === undefined) === (error === undefined) ||
        (code !== undefined && (typeof code !== 'string' || !code || code.length > 4096)) ||
        (error !== undefined && (typeof error !== 'string' || !error || error.length > 256)) ||
        (iss !== undefined && (typeof iss !== 'string' || !iss || iss.length > 4096))) throw invalidCallback();
    const operation = random();
    const pending = await inTransaction(db, async () => {
      await locked(db, id);
      const result = await db.query<ConnectionRow>(`UPDATE credential_oauth SET pending_claim = $6
        WHERE credential_id = $1 AND pending_state = $2 AND pending_user = $3 AND pending_session = $4 AND pending_nonce = $5
          AND pending_claim IS NULL AND pending_expires_at > $7 RETURNING *`,
      [id, digest(state), claims.user_id, session, digest(nonce), operation, d.now()]);
      const row = result.rows[0];
      if (!row?.pending_version) throw invalidCallback();
      const credential = await oauthCredential(d.secrets, db, id);
      const config = credential.config;
      configured(config);
      const { hash } = d.secrets.binding(credential);
      // Return a fixed failure after committing the claim so neither a wrong
      // issuer nor a change in config permits a second exchange attempt.
      if (!row.pending_hash?.equals(hash) || (iss === undefined ? config.iss_supported === true : iss !== config.issuer) || error !== undefined) {
        await destroyFields(db, id, ['oauth_pending'], d.now());
        await db.query(`UPDATE credentials SET oauth_status = COALESCE(oauth_status, '{}') || $2::jsonb WHERE id = $1`,
          [id, JSON.stringify({ connect_result: error !== undefined ? 'denied' : 'invalid_callback' })]);
        return { failure: error !== undefined ? 'oauth.consent_denied' : 'oauth.invalid_callback' } as const;
      }
      const pending = await d.secrets.openForRelease({ credentialId: id, field: 'oauth_pending', pinnedVersion: row.pending_version }, db);
      try {
        const sealed = record(JSON.parse(pending.value.toString('utf8')) as unknown);
        if (sealed.state !== state || sealed.nonce !== nonce || sealed.session !== session || sealed.user !== claims.user_id ||
            sealed.generation !== row.generation || sealed.hash !== hash.toString('base64url') || typeof sealed.verifier !== 'string' ||
            typeof sealed.expires !== 'number' || sealed.expires <= d.now().getTime()) throw invalidCallback();
        await destroyFields(db, id, ['oauth_pending'], d.now());
        return { config, generation: row.generation, verifier: sealed.verifier, hash,
          secret: await this.clientSecret(db, credential, row) };
      } finally {
        pending.value.fill(0);
      }
    });
    if ('failure' in pending) return { failure: pending.failure };
    diagnostic.phase = 'exchange';
    const { config } = pending;
    const tokenForm = new URLSearchParams({ grant_type: 'authorization_code', code: code as string,
      redirect_uri: config.redirect_uri, client_id: config.client_id, code_verifier: pending.verifier, resource: config.resource });
    try {
      const response = await d.provider.request(config.token_endpoint, formRequest(config, tokenForm, pending.secret));
      diagnostic.providerStatus = response.status;
      const pair = tokenPair(response, d.now(), (config.scopes as string[] | undefined) ?? [], pending.secret, code as string);
      await inTransaction(db, async () => {
        await locked(db, id);
        const row = await getConnection(db, id);
        const credential = await oauthCredential(d.secrets, db, id);
        if (row?.pending_claim !== operation || row.generation !== pending.generation ||
            !pending.hash.equals(d.secrets.binding(credential).hash)) throw conflict();
        const version = await writeSecret(d.secrets, db, credential, 'oauth_pending',
          JSON.stringify({ ...pair, generation: row.generation }), claims.user_id);
        const expires = new Date(Math.min(pair.expiresAt.getTime(), d.now().getTime() + 600_000));
        await db.query('UPDATE credential_oauth SET pending_version = $2, pending_expires_at = $3 WHERE credential_id = $1',
          [id, version, expires]);
        const previous = credential.oauth_status?.connected_account;
        const previousIdentity = credential.oauth_status?.connected_account_identity;
        const changed = previousIdentity && pair.accountIdentity
          ? previousIdentity.kind !== pair.accountIdentity.kind || previousIdentity.id !== pair.accountIdentity.id
          : !!previous && previous !== pair.account;
        const signature = d.identity.oauthAccount([
          claims.org_id, d.installId ?? '', id, pending.hash.toString('base64url'), operation,
          pair.account ?? '', String(changed), String(expires.getTime()),
        ]);
        await db.query(`UPDATE credentials SET oauth_status = COALESCE(oauth_status, '{}') || $2::jsonb WHERE id = $1`,
          [id, JSON.stringify({ pending_account: pair.account ?? null, account_confirmation: operation,
            account_changed: changed, account_confirmation_signature: signature,
            account_confirmation_expires_at_ms: expires.getTime(), connect_result: 'account_confirmation_required' })]);
      });
      return { connected: false, awaiting_confirmation: true };
    } catch (error) {
      await this.exchangeFailed(db, id, operation);
      throw error;
    }
  }

  private async exchangeFailed(db: pg.PoolClient, id: string, operation: string) {
    await db.query(`UPDATE credentials c SET oauth_status = (COALESCE(oauth_status, '{}') - 'account_confirmation' - 'pending_account'
      - 'account_confirmation_signature' - 'account_confirmation_expires_at_ms')
      || '{"connect_result":"exchange_failed"}'::jsonb
      FROM credential_oauth o WHERE c.id = $1 AND o.credential_id = c.id AND o.pending_claim = $2`, [id, operation]);
  }

  private async confirmAccount(db: pg.PoolClient, id: string, claims: UserAssertionClaims, pin: unknown, operation: unknown) {
    const { d } = this;
    const session = sessionHash(claims);
    if (typeof operation !== 'string' || !digestPattern.test(operation)) throw conflict();
    let prior;
    try {
      prior = await inTransaction(db, async () => {
        await locked(db, id);
        const row = await getConnection(db, id);
        const credential = await oauthCredential(d.secrets, db, id);
        const config = credential.config;
        configured(config);
        const { hash } = d.secrets.binding(credential);
        requirePin(pin, hash);
        if (row?.pending_claim !== operation || row.pending_user !== claims.user_id || row.pending_session !== session || !row.pending_version ||
            !row.pending_hash?.equals(hash) || !row.pending_expires_at || row.pending_expires_at <= d.now() ||
            credential.oauth_status?.account_confirmation !== operation) throw conflict();
        const opened = await d.secrets.openForRelease({ credentialId: id, field: 'oauth_pending', pinnedVersion: row.pending_version }, db);
        let pair;
        try {
          pair = JSON.parse(opened.value.toString('utf8')) as
            Omit<ReturnType<typeof tokenPair>, 'expiresAt'> & { expiresAt: string; generation: number };
        } finally {
          opened.value.fill(0);
        }
        if (pair.generation !== row.generation) throw conflict();
        const now = d.now();
        const limitMs = Math.min(
          new URL(config.url).hostname === 'mcp.notion.com' ? now.getTime() + 180 * day : Infinity,
          row.client_secret_expires_at?.getTime() ?? Infinity,
        );
        const expires = new Date(pair.expiresAt);
        if (!Number.isFinite(expires.getTime()) || expires <= now || limitMs <= now.getTime()) throw unavailable();
        const limit = Number.isFinite(limitMs) ? new Date(limitMs) : null;
        const grantExpiry = pair.refresh ? limit : new Date(Math.min(expires.getTime(), limitMs));
        const superseded = await this.revocationRequest(db, credential, row, [pair.access, pair.refresh]);
        const generation = row.generation + 1;
        await d.gateway.fenceOAuth(id, generation);
        await destroyFields(db, id, ['oauth_pending', 'access_token', 'refresh_token'], now);
        const accessVersion = await writeSecret(d.secrets, db, credential, 'access_token', pair.access, claims.user_id);
        const refreshVersion = pair.refresh ? await writeSecret(d.secrets, db, credential, 'refresh_token', pair.refresh, claims.user_id) : null;
        await db.query(`UPDATE credential_oauth SET generation = $2, connection_hash = $3,
          pending_state = NULL, pending_claim = NULL, pending_version = NULL, access_version = $4, refresh_version = $5,
          access_issued_at = $6, access_expires_at = $7, grant_expires_at = $8, refreshed_at = $6,
          refresh_operation = NULL, refresh_after = NULL, refresh_failures = 0, forced_refreshes = 0,
          refresh_failure_since = NULL, refresh_failure_warned_at = NULL,
          access_rejected_since = NULL, warned_at = NULL, connection_config = $9, connection_groups = $10
          WHERE credential_id = $1`,
        [id, generation, hash, accessVersion, refreshVersion, now, expires, grantExpiry,
          JSON.stringify(config), JSON.stringify(credential.groups)]);
        await db.query(`UPDATE credentials SET status = 'active',
          oauth_status = (COALESCE(oauth_status, '{}') - 'account_confirmation' - 'pending_account'
      - 'account_confirmation_signature' - 'account_confirmation_expires_at_ms'
              - 'reconnect_reason' - 'refresh_failure_since' - 'refresh_failures' - 'last_refresh_error') || $2::jsonb,
          updated_at = $3 WHERE id = $1`, [id,
          JSON.stringify({ connected_account: pair.account ?? null, connected_account_identity: pair.accountIdentity ?? null, granted_scopes: pair.scopes, access_expires_at: expires,
            grant_expires_at: grantExpiry, access_only: !pair.refresh,
            expiry_warning: grantExpiry !== null && grantExpiry.getTime() - now.getTime() <= warningMs, connect_result: 'connected' }), now]);
        await audit(db, { actor: { kind: 'user', id: claims.user_id }, action: 'credential.oauth_connected', resource_type: 'credential',
          resource_id: id, runner_id: null, group_id: null, detail: { generation, account_changed: credential.oauth_status?.account_changed === true } });
        return { generation, superseded };
      });
    } catch (error) {
      if (!(error instanceof VaultError && ['oauth.operation_conflict', 'oauth.binding_mismatch'].includes(error.code))) {
        await this.exchangeFailed(db, id, operation);
      }
      throw error;
    }
    if (prior.superseded) {
      await d.provider.request(prior.superseded.endpoint, prior.superseded.request, false).catch(() => undefined);
    }
    return { connected: true, generation: prior.generation };
  }

  private async revocationRequest(db: pg.PoolClient, credential: OAuthCredential, row: ConnectionRow,
    preserve: (string | null)[] = []) {
    const { d } = this;
    const config = row.connection_config;
    const groups = row.connection_groups;
    const version = row.refresh_version ?? row.access_version;
    if (!config || !groups || !version || typeof config.revocation_endpoint !== 'string' ||
        typeof config.token_endpoint !== 'string' ||
        !row.connection_hash?.equals(d.secrets.binding({ ...credential, config, groups }).hash)) return;
    try {
      const sameOrigin = new URL(config.revocation_endpoint).origin === new URL(config.token_endpoint).origin;
      const sameClient = credential.config.client_id === config.client_id &&
        credential.config.token_endpoint === config.token_endpoint &&
        credential.config.token_endpoint_auth_method === config.token_endpoint_auth_method;
      if (sameOrigin && !sameClient) return;
      const secret = sameOrigin ? await this.clientSecret(db, credential, row) : undefined;
      const opened = await d.secrets.openForRelease({ credentialId: credential.id, field: row.refresh_version ? 'refresh_token' : 'access_token',
        pinnedVersion: version, revocationBinding: { config, groups } }, db);
      try {
        const token = opened.value.toString('utf8');
        if (preserve.includes(token)) return;
        return { endpoint: config.revocation_endpoint, request: formRequest(config,
          new URLSearchParams({ token, token_type_hint: row.refresh_version ? 'refresh_token' : 'access_token',
            ...(sameOrigin ? { client_id: config.client_id as string } : {}) }), secret) };
      } finally {
        opened.value.fill(0);
      }
    } catch {
      // Revocation is best effort where the provider or client no longer supports it.
    }
  }

  private async pendingRevocation(db: pg.PoolClient, credential: OAuthCredential, row: ConnectionRow, preserveActive = true) {
    if (!row.pending_version || !row.pending_hash?.equals(this.d.secrets.binding(credential).hash)) return;
    const config = credential.config;
    if (typeof config.revocation_endpoint !== 'string' || typeof config.token_endpoint !== 'string') return;
    try {
      const opened = await this.d.secrets.openForRelease({
        credentialId: credential.id, field: 'oauth_pending', pinnedVersion: row.pending_version,
      }, db);
      let pair: Record<string, unknown>;
      try {
        pair = record(JSON.parse(opened.value.toString('utf8')) as unknown);
      } finally {
        opened.value.fill(0);
      }
      const token = pair.refresh ?? pair.access;
      if (typeof token !== 'string') return; // An authorization started without a grant.
      if (preserveActive) {
        for (const [field, version] of [['access_token', row.access_version], ['refresh_token', row.refresh_version]] as const) {
          if (!version || !row.connection_config || !row.connection_groups) continue;
          const active = await this.d.secrets.openForRelease({ credentialId: credential.id, field, pinnedVersion: version,
            revocationBinding: { config: row.connection_config, groups: row.connection_groups } }, db);
          try {
            if (active.value.toString('utf8') === token) return;
          } finally {
            active.value.fill(0);
          }
        }
      }
      const sameOrigin = new URL(config.revocation_endpoint).origin === new URL(config.token_endpoint).origin;
      const secret = sameOrigin ? await this.clientSecret(db, credential, row) : undefined;
      return { endpoint: config.revocation_endpoint, request: formRequest(config,
        new URLSearchParams({ token, token_type_hint: pair.refresh ? 'refresh_token' : 'access_token',
          ...(sameOrigin ? { client_id: config.client_id as string } : {}) }), secret) };
    } catch {
      // An expired or replaced client can make upstream revocation unavailable.
    }
  }

  private async revokeGrant(material: { endpoint: string; request: ReturnType<typeof formRequest> } | undefined) {
    return material && this.d.provider.request(material.endpoint, material.request, false).catch(() => undefined);
  }

  protected async discardExpiredAuthorizations() {
    const { d } = this;
    const { rows } = await d.pool.query<{ credential_id: string }>(`SELECT credential_id FROM credential_oauth
      WHERE pending_version IS NOT NULL AND pending_expires_at <= $1 ORDER BY pending_expires_at LIMIT 100`, [d.now()]);
    for (const { credential_id: id } of rows) {
      await withRefreshLock(d.pool, id, async (db) => {
        const material = await inTransaction(db, async () => {
          await locked(db, id);
          const row = await getConnection(db, id);
          if (!row?.pending_version || !row.pending_expires_at || row.pending_expires_at > d.now()) return;
          const credential = await d.secrets.credential(id, db);
          const discarded = await this.pendingRevocation(db, { ...credential, config: record(credential.config) }, row);
          await destroyFields(db, id, ['oauth_pending'], d.now());
          await db.query(`UPDATE credential_oauth SET pending_state = NULL, pending_claim = NULL,
            pending_version = NULL WHERE credential_id = $1`, [id]);
          await db.query(`UPDATE credentials SET oauth_status = (COALESCE(oauth_status, '{}') - 'account_confirmation'
            - 'pending_account' - 'account_confirmation_signature' - 'account_confirmation_expires_at_ms')
            || '{"connect_result":"expired"}'::jsonb WHERE id = $1 AND oauth_status->>'connect_result' IN
            ('pending', 'account_confirmation_required')`, [id]);
          return discarded;
        });
        await this.revokeGrant(material);
      });
    }
  }

  protected async clientSecret(db: pg.PoolClient, credential: OAuthCredential, row: ConnectionRow): Promise<string | undefined> {
    if (credential.config.token_endpoint_auth_method === 'none') return undefined;
    if (row.client_secret_expires_at && row.client_secret_expires_at <= this.d.now()) throw unavailable();
    const opened = await this.d.secrets.openForRelease({ credentialId: credential.id, field: 'client_secret' }, db);
    try {
      return opened.value.toString('utf8');
    } finally {
      opened.value.fill(0);
    }
  }

  async disconnect(id: string, claims: UserAssertionClaims) {
    requireAdmin(claims);
    return withRefreshLock(this.d.pool, id, (db) => this.revokeConnection(db, id, claims));
  }

  private async revokeConnection(db: pg.PoolClient, id: string, claims: UserAssertionClaims) {
    const { d } = this;
    const pending = await inTransaction(db, async () => {
      await locked(db, id);
      const credential = await oauthCredential(d.secrets, db, id);
      const row = await getConnection(db, id);
      if (!row) return undefined;
      if (row.purge_until) {
        return { ...await this.finishPurge(db, id, row), existed: false, material: undefined, discarded: undefined };
      }
      if (!row.access_version && credential.oauth_status?.connect_result === 'disconnected') return undefined;
      const now = d.now();
      const generation = row.generation + 1;
      const material = await this.revocationRequest(db, credential, row);
      const discarded = await this.pendingRevocation(db, credential, row, !material);
      const acknowledged = await d.gateway.fenceOAuth(id, generation).then(() => true, () => false);
      // Include previously issued material that may outlive the current token.
      // A removal attempt can have failed even for a release marked ended.
      const { rows: [prior] } = await db.query<{ expires: Date | null }>(
        "SELECT max(expires_at) AS expires FROM releases WHERE credential_id = $1 AND delivery = 'mcp'", [id]);
      const remaining = Math.max(0, (row.access_expires_at?.getTime() ?? 0) - now.getTime(),
        (prior?.expires?.getTime() ?? 0) - now.getTime());
      const purgeUntil = !acknowledged && remaining > 0 ? new Date(now.getTime() + remaining) : null;
      await destroyFields(db, id, ['oauth_pending'], now);
      await db.query(`UPDATE credential_oauth SET generation = $2, connection_hash = NULL, connection_config = NULL, connection_groups = NULL, pending_claim = NULL, pending_state = NULL,
        pending_version = NULL, access_version = NULL, refresh_version = NULL, access_expires_at = NULL, grant_expires_at = NULL,
        refresh_operation = NULL, refresh_after = NULL, refresh_failures = 0, forced_refreshes = 0,
          refresh_failure_since = NULL, refresh_failure_warned_at = NULL,
        access_rejected_since = NULL, warned_at = NULL, purge_until = $3 WHERE credential_id = $1`, [id, generation, purgeUntil]);
      await db.query(`UPDATE credentials SET status = 'needs_secret',
        oauth_status = jsonb_strip_nulls(jsonb_build_object('discovery_candidate', oauth_status->'discovery_candidate')) || $3::jsonb,
        updated_at = $2 WHERE id = $1`,
        [id, now, JSON.stringify({ connect_result: purgeUntil ? 'pending_purge' : 'disconnected', purge_until: purgeUntil })]);
      await audit(db, { actor: { kind: 'user', id: claims.user_id }, action: 'credential.oauth_disconnected', resource_type: 'credential',
        resource_id: id, runner_id: null, group_id: null, detail: { generation } });
      return { material, discarded, existed: row.access_version !== null, purged: !purgeUntil, remaining: purgeUntil ? remaining : 0 };
    });
    const discarded = pending?.discarded?.request.body === pending?.material?.request.body
      ? undefined : await this.revokeGrant(pending?.discarded);
    if (!pending?.existed) {
      const credential = await d.secrets.credential(id, db);
      return { revoked: discarded ? discarded.status >= 200 && discarded.status < 300 : credential.oauth_status?.upstream_revoked ?? !pending, gateway_purged: pending?.purged ?? true,
        pending_purge: pending ? !pending.purged : false, access_token_remaining_ms: pending?.remaining ?? 0 };
    }
    const result = await this.revokeGrant(pending.material);
    // Local destruction follows the revocation attempt even if the provider
    // refused it. A restart sweeps the sealed versions of disconnected rows.
    await inTransaction(db, async () => {
      await locked(db, id);
      await destroyFields(db, id, ['access_token', 'refresh_token'], d.now());
    });
    const revoked = result !== undefined && result.status >= 200 && result.status < 300;
    await db.query('UPDATE credentials SET oauth_status = oauth_status || $2::jsonb WHERE id = $1',
      [id, JSON.stringify({ upstream_revoked: revoked })]);
    return { revoked, gateway_purged: pending.purged, pending_purge: !pending.purged, access_token_remaining_ms: pending.remaining };
  }

  private async finishPurge(db: pg.PoolClient, id: string, row: ConnectionRow) {
    const remaining = Math.max(0, (row.purge_until?.getTime() ?? 0) - this.d.now().getTime());
    if (remaining && !await this.d.gateway.fenceOAuth(id, row.generation).then(() => true, () => false)) {
      return { purged: false, remaining };
    }
    await db.query('UPDATE credential_oauth SET purge_until = NULL WHERE credential_id = $1', [id]);
    await db.query(`UPDATE credentials SET oauth_status = oauth_status - 'purge_until' || '{"connect_result":"disconnected"}'::jsonb
      WHERE id = $1 AND oauth_status->>'connect_result' = 'pending_purge'`, [id]);
    return { purged: true, remaining: 0 };
  }

  protected async retryPurges() {
    const { rows } = await this.d.pool.query<{ credential_id: string }>(
      'SELECT credential_id FROM credential_oauth WHERE purge_until IS NOT NULL ORDER BY purge_until LIMIT 100');
    for (const { credential_id: id } of rows) {
      await withRefreshLock(this.d.pool, id, (db) => inTransaction(db, async () => {
        await locked(db, id);
        const row = await getConnection(db, id);
        if (row?.purge_until) await this.finishPurge(db, id, row);
      }));
    }
  }
}
