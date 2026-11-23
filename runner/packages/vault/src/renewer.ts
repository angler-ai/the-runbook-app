// Grant renewal (design §7.5): the vault renews each live release's grant
// at 70% of its lifetime with its own release token. When the core refuses
// (the claim moved on: taken over, cancelled, lapsed), the secret can no
// longer be opened, or the credential was revoked, the release ends: the
// gateway drops the material, the core records the revocation, and nothing
// is renewed again. Transient failures retry until the grant expires, and the
// gateway drops material at expiry on its own (fail closed).
import type pg from 'pg';
import { retryDelay } from '@runbook/runner-shared/retry';
import { sandboxIdForClaim } from '@runbook/runner-shared/capability';
import type { ClaimFence } from '@runbook/runner-shared/contracts/internal';
import type { Delivery, GrantClaims } from '@runbook/runner-shared/contracts/core';
import { errorFields, type Logger } from '@runbook/runner-shared/log';
import { audit } from './audit.ts';
import type { CoreGrants } from './core.ts';
import { VaultError } from './errors.ts';
import type { GatewayClient } from './gateway.ts';
import { ATTACHED_VERTEX_PREFIX } from '@runbook/runner-shared/vertex-binding';
import { checkRunnerPolicy, notAllowed } from './policy.ts';
import type { OAuthAccessService } from './oauth/access.ts';
import { unavailable } from './oauth/state.ts';
import type { SecretStore } from './secrets.ts';
import { groupAllowed } from './types/index.ts';
import { ATTACHED_VERTEX_FIELD, checkVertexPolicy, type VertexIdentity } from './vertex-identity.ts';

export const RENEW_AT_FRACTION = 0.7;
const TRANSIENT = new Set(['core.unavailable', 'release.gateway_unavailable', 'oauth.refresh_retry', 'secret_manager.retry', 'vertex.adc_retry']);

export interface LiveRelease {
  releaseId: string;
  stepRunId: string;
  credentialId: string;
  credentialVersion: number;
  field: string;
  delivery: Delivery;
  /** The capability's claims the release was made under. */
  claims: Pick<
    GrantClaims,
    'claim_id' | 'generation' | 'runner_id' | 'group_id' | 'agent_id' | 'agent_version' | 'project_id'
  >;
  vertexClaims?: Pick<GrantClaims, 'reach' | 'model_profile' | 'model_id' | 'connections'>;
  /** Unix seconds. */
  issuedAt: number;
  expiresAt: number;
  materialExpiry?: number;
  gatewayEpoch?: string | undefined;
  retry?: { at: number; failures: number };
}

export function renewAtMs(issuedAt: number, expiresAt: number): number {
  return (issuedAt + RENEW_AT_FRACTION * (expiresAt - issuedAt)) * 1000;
}

export interface RenewerDeps {
  pool: pg.Pool;
  core: CoreGrants;
  gateway: GatewayClient;
  secrets: SecretStore;
  oauth?: OAuthAccessService;
  vertex?: VertexIdentity;
  now: () => Date;
  log: Logger;
}

export class Renewer {
  private readonly live = new Map<string, LiveRelease>();
  private readonly probes = new Map<string, { next: number; delay: number }>();
  private readonly d: RenewerDeps;
  private running: Promise<void> | undefined;

  constructor(deps: RenewerDeps) {
    this.d = deps;
  }

  track(release: LiveRelease): void {
    this.live.set(release.releaseId, release);
  }

  get size(): number {
    return this.live.size;
  }

  /** After a restart: pick up releases still live in the database and renew them at once. */
  async loadLive(): Promise<number> {
    const { rows } = await this.d.pool.query<{
      id: string;
      step_run_id: string;
      credential_id: string;
      credential_version: number;
      delivery: Delivery;
      claim_id: string;
      generation: number;
      runner_id: string;
      group_id: string;
      agent_id: string;
      agent_version: string;
      project_id: string;
      field: string | null;
      created_at: Date;
      expires_at: Date;
    }>(`SELECT * FROM releases WHERE status = 'live' AND expires_at > $1`, [this.d.now()]);
    for (const r of rows) {
      if (r.field !== ATTACHED_VERTEX_FIELD || !r.credential_id.startsWith(ATTACHED_VERTEX_PREFIX)) {
        const credential = await this.d.secrets.credential(r.credential_id).catch(() => undefined);
        if (!credential) continue;
      }
      this.track({
        releaseId: r.id,
        stepRunId: r.step_run_id,
        credentialId: r.credential_id,
        credentialVersion: r.credential_version,
        field: r.field ?? 'key',
        delivery: r.delivery,
        claims: {
          claim_id: r.claim_id,
          generation: r.generation,
          runner_id: r.runner_id,
          group_id: r.group_id,
          agent_id: r.agent_id,
          agent_version: r.agent_version,
          project_id: r.project_id,
        },
        issuedAt: 0,
        expiresAt: Math.floor(r.expires_at.getTime() / 1000),
      });
    }
    return rows.length;
  }

  /** Renew what is due and end what was revoked. One pass at a time. */
  runDue(): Promise<void> {
    this.running ??= this.pass().finally(() => {
      this.running = undefined;
    });
    return this.running;
  }

  async endClaim(claim: ClaimFence, reason: string): Promise<void> {
    await this.d.gateway.endClaim(claim);
    const id = sandboxIdForClaim(claim);
    await Promise.all([...this.live.values()].filter((release) =>
      sandboxIdForClaim({ sub: release.stepRunId, ...release.claims }) === id)
      .map((release) => this.end(release, reason, 'revoked').catch((error: unknown) =>
        this.d.log.error('could not record claim release end', { release_id: release.releaseId, ...errorFields(error) }),
      )));
  }

  async oauthRejected(claim: GrantClaims, releaseId: string, tokenVersion: number): Promise<void> {
    const release = this.live.get(releaseId);
    if (!release || release.field !== 'access_token' || release.stepRunId !== claim.sub ||
        release.claims.claim_id !== claim.claim_id || release.claims.generation !== claim.generation ||
        release.claims.runner_id !== claim.runner_id || release.expiresAt * 1000 <= this.d.now().getTime()) throw unavailable();
    await this.renew(release, tokenVersion);
  }

  private async checkClaim(claim: ClaimFence): Promise<boolean> {
    const { sub, claim_id, runner_id, generation } = claim;
    try {
      await this.d.core.grant(sub, { kind: 'capability', claim_id, runner_id, generation }, claim);
    } catch (error) {
      if (!(error instanceof VaultError) || (error.status !== 403 && error.status !== 409)) return false;
      await this.endClaim(claim, error.code);
    }
    return true;
  }

  private async pass(): Promise<void> {
    if (this.live.size === 0) {
      this.probes.clear();
      return;
    }
    // One probe per pass, independent of the number of live releases.
    // An unreachable gateway cannot use new material, so defer the grant
    // requests until it is back.
    let epoch: string | undefined;
    let gatewayAvailable = true;
    try {
      epoch = await this.d.gateway.epoch();
    } catch {
      gatewayAvailable = false;
    }
    const ids = [...new Set([...this.live.values()].map((r) => r.credentialId))];
    const { rows } = await this.d.pool.query<{ id: string }>(
      `SELECT id FROM credentials WHERE id = ANY($1) AND status IN ('revoked', 'reconnect_required')`,
      [ids],
    );
    const revoked = new Set(rows.map((r) => r.id));
    const claims = new Map<string, ClaimFence>();
    for (const item of this.live.values()) {
      const claim = { sub: item.stepRunId, ...item.claims };
      if (!revoked.has(item.credentialId)) claims.set(sandboxIdForClaim(claim), claim);
    }
    for (const id of this.probes.keys()) if (!claims.has(id)) this.probes.delete(id);
    for (const [id, claim] of claims) {
      const now = this.d.now().getTime();
      const previous = this.probes.get(id);
      if (!gatewayAvailable || (previous && previous.next > now)) continue;
      const delay = Math.min(60_000, (previous?.delay ?? 5_000) * 2);
      let checked = false;
      try {
        checked = await this.checkClaim(claim);
      } catch (error) {
        this.d.log.error('could not end a moved claim', errorFields(error));
      }
      this.probes.set(id, { next: now + (checked ? delay : retryDelay(0, delay, delay)), delay });
    }
    const now = this.d.now().getTime();
    for (const release of [...this.live.values()]) {
      // One release's failure (an audit entry that can't be written, the
      // database) never stops the others from being renewed or ended.
      try {
        if (release.field === ATTACHED_VERTEX_FIELD) {
          try {
            await checkVertexPolicy(this.d.vertex, this.d.pool, { ...release.claims, ...release.vertexClaims }, release.credentialId, release.delivery);
          } catch (error) {
            if (!(error instanceof VaultError) || error.code !== 'release.not_allowed') throw error;
            await this.end(release, error.code, 'revoked');
            continue;
          }
        }
        if (revoked.has(release.credentialId)) await this.end(release, 'credential.revoked', 'revoked');
        else if (now >= release.expiresAt * 1000 && (!gatewayAvailable || release.retry !== undefined)) {
          await this.end(release, 'grant.expired', 'expired');
        } else if (gatewayAvailable && (!release.retry || release.retry.at <= now) &&
            ((epoch !== undefined && epoch !== release.gatewayEpoch) ||
             now >= renewAtMs(release.issuedAt, Math.min(release.expiresAt, (release.materialExpiry ?? Infinity) / 1000)))) {
          await this.renew(release);
        }
      } catch (e) {
        this.d.log.error('could not renew or end a release', { release_id: release.releaseId, ...errorFields(e) });
      }
    }
  }

  private async renew(r: LiveRelease, observedVersion?: number): Promise<void> {
    const c = r.claims;
    try {
      const { claims } = await this.d.core.grant(
        r.stepRunId,
        {
          kind: 'credential',
          claim_id: c.claim_id,
          generation: c.generation,
          runner_id: c.runner_id,
          credential_id: r.credentialId,
          credential_version: r.credentialVersion,
          delivery: r.delivery,
        },
        { ...c, credential_id: r.credentialId, credential_version: r.credentialVersion, delivery: r.delivery },
      );
      if (r.field === ATTACHED_VERTEX_FIELD) {
        if (!this.d.vertex) throw notAllowed();
        const { binding, serviceAccount } = await checkVertexPolicy(this.d.vertex, this.d.pool, claims, r.credentialId, r.delivery);
        const access = await this.d.vertex.access(serviceAccount, claims.exp * 1000);
        const current = await checkVertexPolicy(this.d.vertex, this.d.pool, claims, r.credentialId, r.delivery);
        if (current.binding.endpoint !== binding.endpoint) throw notAllowed();
        r.gatewayEpoch = await this.d.gateway.push({
          release_id: r.releaseId, step_run_id: r.stepRunId, claim_id: claims.claim_id, generation: claims.generation,
          runner_id: claims.runner_id, credential_id: r.credentialId, delivery: r.delivery, binding: { ...binding },
          material: access.material, expires_at: new Date(access.expiresAt).toISOString(),
        });
        r.materialExpiry = access.expiresAt;
        r.vertexClaims = claims;
      } else if (r.field === 'access_token') {
        if (!this.d.oauth) throw unavailable();
        await this.d.oauth.ensureAccess(r.credentialId, 'renewal', observedVersion);
        await this.d.core.grant(r.stepRunId, {
          kind: 'capability', claim_id: c.claim_id, generation: c.generation, runner_id: c.runner_id,
        }, c);
        r.materialExpiry = await this.d.oauth.withAccess(r.credentialId, claims.exp * 1000, async (access, db) => {
          if (access.generation !== r.credentialVersion) throw unavailable();
          await checkRunnerPolicy(db, claims, await this.d.secrets.credential(r.credentialId, db), r.delivery);
          r.gatewayEpoch = await this.d.gateway.push({
            release_id: r.releaseId, step_run_id: r.stepRunId, claim_id: claims.claim_id, generation: claims.generation,
            runner_id: claims.runner_id, credential_id: r.credentialId, delivery: r.delivery, binding: access.binding,
            material: access.material, expires_at: new Date(access.expiresAt).toISOString(),
          });
          return access.expiresAt;
        });
      } else {
        const opened = await this.d.secrets.openForRelease({
          credentialId: r.credentialId,
          field: r.field,
          pinnedVersion: r.credentialVersion,
          authorize: (credential) => checkRunnerPolicy(this.d.pool, claims, credential, r.delivery),
        });
        try {
          await checkRunnerPolicy(this.d.pool, claims, await this.d.secrets.credential(r.credentialId), r.delivery);
          if (!groupAllowed(opened.groups, c.group_id)) throw new VaultError(403, 'release.not_allowed', 'group not allowed');
          r.gatewayEpoch = await this.d.gateway.push({
            release_id: r.releaseId,
            step_run_id: r.stepRunId,
            claim_id: claims.claim_id,
            generation: claims.generation,
            runner_id: claims.runner_id,
            credential_id: r.credentialId,
            delivery: r.delivery,
            binding: opened.binding,
            material: { fields: { [opened.field]: opened.value.toString('utf8') } },
            expires_at: new Date(claims.exp * 1000).toISOString(),
          });
        } finally {
          opened.value.fill(0);
        }
      }
      delete r.retry;
      r.issuedAt = claims.iat;
      r.expiresAt = claims.exp;
      await this.d.pool.query(`UPDATE releases SET expires_at = $2, renewed_at = $3, grant_jti = $4 WHERE id = $1`, [
        r.releaseId,
        new Date(claims.exp * 1000),
        this.d.now(),
        claims.jti,
      ]);
      this.d.log.debug('grant renewed', { release_id: r.releaseId, step_run_id: r.stepRunId, expires_at: claims.exp });
    } catch (e) {
      const code = e instanceof VaultError ? e.code : 'internal';
      if (TRANSIENT.has(code) || code === 'internal') {
        this.d.log.warn('grant renewal failed; will retry', { release_id: r.releaseId, ...errorFields(e) });
        const now = this.d.now().getTime();
        const failures = (r.retry?.failures ?? 0) + 1;
        r.retry = { failures, at: now + retryDelay(failures - 1, 5_000, 60_000) };
        if (now >= r.expiresAt * 1000) await this.end(r, 'grant.expired', 'expired');
        return;
      }
      await this.end(r, code, 'revoked');
    }
  }

  /** End a release now: drop the material, tell the core, record it. Throws if it can't be audited. */
  async end(r: LiveRelease, reason: string, status: 'revoked' | 'expired'): Promise<void> {
    if (!this.live.delete(r.releaseId)) return;
    await this.d.gateway.remove(r.releaseId).catch((e: unknown) =>
      this.d.log.error('could not delete material at the gateway', { release_id: r.releaseId, ...errorFields(e) }),
    );
    await this.d.core.revoked(r.stepRunId, r.credentialId, reason).catch((e: unknown) =>
      this.d.log.error('could not record the revocation in the core', { release_id: r.releaseId, ...errorFields(e) }),
    );
    await this.d.pool.query(`UPDATE releases SET status = $2, ended_at = $3, reason = $4 WHERE id = $1`, [
      r.releaseId,
      status,
      this.d.now(),
      reason,
    ]);
    await audit(this.d.pool, {
      actor: { kind: 'process', id: 'vault' },
      action: 'credential.revoked',
      resource_type: 'credential',
      resource_id: r.credentialId,
      group_id: r.claims.group_id,
      runner_id: r.claims.runner_id,
      detail: { release_id: r.releaseId, step_run_id: r.stepRunId, version: r.credentialVersion, reason },
    });
    this.d.log.info('release ended', { release_id: r.releaseId, step_run_id: r.stepRunId, reason });
  }
}
