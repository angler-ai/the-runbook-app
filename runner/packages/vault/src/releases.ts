// Capabilities, releases and connection tests (design §7.5, §7.6, §7.4).
//
// - POST /internal/capability (worker): ask the core for a capability grant
//   for a claimed step run and hand it back after verifying it.
// - POST /internal/release (worker): check runner policy first (no core
//   call for a credential the agent version doesn't list), then a credential
//   grant from the core, then unwrap against the recomputed binding and push
//   the material to the gateway. The response carries IDs and the expiry,
//   never material.
// - POST /internal/test (api): a short test release to the gateway, which
//   calls the bound destination; the answer is pass or a fixed error class.
import { randomBytes } from 'node:crypto';
import type pg from 'pg';
import { DELIVERIES, type Delivery, type GrantClaims, type UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import type {
  CapabilityRequest,
  CapabilityResponse,
  GatewayTestResponse,
  McpDiscoveryRequest,
  ReleaseRequest,
  ReleaseResponse,
  TestRequest,
  TestResponse,
} from '@runbook/runner-shared/contracts/internal';
import { withTransaction } from '@runbook/runner-shared/db';
import { JwtError } from '@runbook/runner-shared/jwt';
import { errorFields, type Logger } from '@runbook/runner-shared/log';
import { AUDIT_UNAVAILABLE, audit } from './audit.ts';
import type { CoreGrants } from './core.ts';
import { VaultError } from './errors.ts';
import type { GatewayClient } from './gateway.ts';
import { checkRunnerPolicy, notAllowed } from './policy.ts';
import { Renewer } from './renewer.ts';
import type { OAuthAccess, OAuthAccessService } from './oauth/access.ts';
import { getConnection, unavailable } from './oauth/state.ts';
import type { SecretStore } from './secrets.ts';
import { groupAllowed, typeImpl } from './types/index.ts';

const ID = /^[A-Za-z0-9_.:-]{1,128}$/;
/** How long a connection test's material lives at the gateway. */
const TEST_RELEASE_SECONDS = 60;

export interface ReleaseDeps {
  pool: pg.Pool;
  core: CoreGrants;
  gateway: GatewayClient;
  secrets: SecretStore;
  oauth?: OAuthAccessService;
  now: () => Date;
  log: Logger;
}

const id = (prefix: string) => `${prefix}_${randomBytes(12).toString('hex')}`;
const iso = (seconds: number) => new Date(seconds * 1000).toISOString();

function bad(what: string): never {
  throw new VaultError(400, 'request.invalid', `invalid request: ${what}`);
}

export function classifyTest(result: GatewayTestResponse): TestResponse {
  if (result.outcome !== 'response') return { ok: false, error_class: result.outcome };
  if (result.status >= 200 && result.status < 300) return { ok: true };
  if (result.status === 401) return { ok: false, error_class: 'unauthorized' };
  if (result.status === 403) return { ok: false, error_class: 'forbidden' };
  return { ok: false, error_class: 'unexpected-response' };
}

export class ReleaseService {
  readonly renewer: Renewer;
  /** Also used to build a second renewer, as a restarted vault would. */
  readonly d: ReleaseDeps;

  constructor(deps: ReleaseDeps) {
    this.d = deps;
    this.renewer = new Renewer(deps);
  }

  private async runnerGroup(runnerId: string): Promise<string> {
    const { rows } = await this.d.pool.query<{ group_id: string }>('SELECT group_id FROM runners WHERE runner_id = $1', [
      runnerId,
    ]);
    const group = rows[0]?.group_id;
    if (!group) throw new VaultError(403, 'capability.unknown_runner', 'the runner is not registered with this control plane');
    return group;
  }

  async capability(body: Partial<CapabilityRequest>, runnerId?: string): Promise<CapabilityResponse> {
    if (typeof body?.step_run_id !== 'string' || !ID.test(body.step_run_id)) bad('step_run_id');
    if (typeof body.claim_id !== 'string' || !ID.test(body.claim_id)) bad('claim_id');
    if (!Number.isSafeInteger(body.generation) || (body.generation ?? -1) < 0) bad('generation');
    if (typeof body.runner_id !== 'string' || !ID.test(body.runner_id)) bad('runner_id');
    if (runnerId && body.runner_id !== runnerId) throw new VaultError(403, 'release.runner_denied', 'runner mismatch');
    const group = await this.runnerGroup(body.runner_id);
    const { token, claims } = await this.d.core.grant(
      body.step_run_id,
      { kind: 'capability', claim_id: body.claim_id, generation: body.generation as number, runner_id: body.runner_id },
      { claim_id: body.claim_id, generation: body.generation as number, runner_id: body.runner_id, group_id: group },
    );
    this.d.log.info('capability issued', { step_run_id: claims.sub, runner_id: claims.runner_id, expires_at: claims.exp });
    return { capability: token, expires_at: iso(claims.exp) };
  }

  private async verifyCapability(token: unknown): Promise<GrantClaims> {
    if (typeof token !== 'string' || token === '') {
      throw new VaultError(401, 'release.invalid_capability', 'a valid capability is required');
    }
    let claims: GrantClaims;
    try {
      claims = await this.d.core.verify(token, { kind: 'capability' });
    } catch (e) {
      if (e instanceof JwtError) throw new VaultError(401, 'release.invalid_capability', 'a valid capability is required');
      throw e;
    }
    if ((await this.runnerGroup(claims.runner_id)) !== claims.group_id) {
      throw new VaultError(401, 'release.invalid_capability', 'a valid capability is required');
    }
    return claims;
  }

  private async audit(action: string, credentialId: string, cap: GrantClaims | undefined, detail: Record<string, unknown>) {
    await audit(this.d.pool, {
      actor: { kind: 'process', id: 'vault' },
      action,
      resource_type: 'credential',
      resource_id: credentialId,
      group_id: cap?.group_id ?? null,
      runner_id: cap?.runner_id ?? null,
      detail,
    });
  }

  async revoke(credentialId: string, destroy: boolean, claims: UserAssertionClaims): Promise<void> {
    const current = await this.d.secrets.credential(credentialId);
    if (current.type === 'mcp-oauth' && current.status !== 'revoked' && this.d.oauth) {
      await this.d.oauth.disconnect(credentialId, claims);
    }
    await withTransaction(this.d.pool, async (client) => {
      await client.query('SELECT pg_advisory_xact_lock(hashtextextended($1, 7419006))', [credentialId]);
      const credential = await this.d.secrets.credential(credentialId, client, true);
      const orgAdmin = (!destroy || claims.principal === 'human') && (claims.org_role === 'owner' || claims.org_role === 'admin');
      const projectAdmin = !destroy && credential.project_id && Object.hasOwn(claims.project_roles, credential.project_id) &&
        claims.project_roles[credential.project_id] === 'admin';
      if (!claims.scopes.includes('runner:admin') || (!orgAdmin && !projectAdmin)) throw notAllowed();
      const now = this.d.now();
      await client.query(
        `UPDATE credentials SET status = 'revoked', revoked_at = COALESCE(revoked_at, $2), updated_at = $2,
           deleted_at = CASE WHEN $3 THEN COALESCE(deleted_at, $2) ELSE deleted_at END,
           secret_meta = CASE WHEN $3 THEN '{}' ELSE secret_meta END WHERE id = $1`, [credential.id, now, destroy],
      );
      if (destroy) await client.query(
        `UPDATE credential_secret_versions SET wrapped_dek = NULL, destroyed_at = $2
         WHERE credential_id = $1 AND destroyed_at IS NULL`, [credential.id, now],
      );
      await audit(client, {
        actor: claims.principal === 'human' ? { kind: 'user', id: claims.user_id } :
          { kind: 'agent', id: claims.user_id, sponsor_id: claims.sponsor_id ?? undefined },
        action: destroy ? 'credential.deleted' : 'credential.revoked',
        resource_type: 'credential', resource_id: credential.id, group_id: null, runner_id: null, detail: {},
      });
    });
    await this.d.gateway.revokeCredential(credentialId);
    await this.renewer.runDue();
  }

  async oauthRejected(body: unknown, runnerId: string): Promise<void> {
    const input = body as { capability?: unknown; release_id?: unknown; token_version?: unknown } | undefined;
    const claim = await this.verifyCapability(input?.capability);
    if (claim.runner_id !== runnerId || typeof input?.release_id !== 'string' || !ID.test(input.release_id) ||
        typeof input.token_version !== 'number' || !Number.isSafeInteger(input.token_version) || input.token_version < 1) throw notAllowed();
    await this.renewer.oauthRejected(claim, input.release_id, input.token_version);
  }

  async endClaim(body: { capability?: unknown } | undefined, runnerId?: string): Promise<void> {
    const claims = await this.verifyCapability(body?.capability);
    if (runnerId && claims.runner_id !== runnerId) {
      throw new VaultError(403, 'release.invalid_capability', 'claim is not held by this runner');
    }
    await this.renewer.endClaim(claims, 'claim.ended');
  }

  async release(body: Partial<ReleaseRequest>, runnerId: string): Promise<ReleaseResponse> {
    const cap = await this.verifyCapability(body?.capability);
    if (cap.runner_id !== runnerId) throw new VaultError(403, 'release.runner_denied', 'runner mismatch');
    if (typeof body?.credential_id !== 'string' || !ID.test(body.credential_id)) bad('credential_id');
    if (!DELIVERIES.includes(body.delivery as Delivery)) bad('delivery');
    const delivery = body.delivery as Delivery;
    const credentialId = body.credential_id;

    // 1. Runner policy, before anything is asked of the core.
    let field: string;
    let version: number;
    try {
      const credential = await this.d.secrets.credential(credentialId).catch(() => {
        throw notAllowed();
      });
      await checkRunnerPolicy(this.d.pool, cap, credential, delivery);
      const impl = typeImpl(credential.type);
      const oauth = credential.type === 'mcp-oauth';
      if (!oauth && impl.secretFields.length !== 1) throw new VaultError(400, 'release.unsupported_type', 'type not releasable in v1');
      field = oauth ? 'access_token' : impl.secretFields[0] as string;
      if (oauth && credential.status !== 'active') throw unavailable();
      const latest = oauth ? (await getConnection(this.d.pool, credentialId))?.generation :
        await this.d.secrets.latestVersion(credentialId, field);
      if (latest === undefined) throw new VaultError(409, 'secret.not_set', 'the credential has no secret yet');
      version = latest;
    } catch (e) {
      const code = e instanceof VaultError ? e.code : 'internal';
      this.d.log.warn('release denied', { step_run_id: cap.sub, credential_id: credentialId, code });
      await this.audit('credential.release_denied', credentialId, cap, { step_run_id: cap.sub, delivery, code });
      throw e;
    }

    // 2. A grant from the core, verified, pinned to this version.
    const { claims } = await this.d.core.grant(
      cap.sub,
      {
        kind: 'credential',
        claim_id: cap.claim_id,
        generation: cap.generation,
        runner_id: cap.runner_id,
        credential_id: credentialId,
        credential_version: version,
        delivery,
      },
      {
        claim_id: cap.claim_id,
        generation: cap.generation,
        runner_id: cap.runner_id,
        group_id: cap.group_id,
        agent_id: cap.agent_id,
        agent_version: cap.agent_version,
        project_id: cap.project_id,
        credential_id: credentialId,
        credential_version: version,
        delivery,
      },
    );

    // 3. Unwrap against the recomputed binding, push to the gateway and
    // record the release. The core has recorded it; any failure from here is
    // reported back, and material already pushed is withdrawn (a release that
    // can't be audited doesn't stand).
    const releaseId = id('rel');
    let pushed = false;
    let gatewayEpoch: string | undefined;
    let materialExpiry = claims.exp * 1000;
    try {
      const deliver = async (material: OAuthAccess['material'], binding: Record<string, unknown>, expiry: number, db: pg.Pool | pg.PoolClient) => {
        materialExpiry = expiry;
        await db.query(
          `INSERT INTO releases (id, step_run_id, claim_id, generation, group_id, runner_id, project_id, agent_id,
             agent_version, credential_id, field, credential_version, delivery, grant_jti, status, expires_at, created_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'live', $15, $16)`,
          [
            releaseId,
            claims.sub,
            claims.claim_id,
            claims.generation,
            claims.group_id,
            claims.runner_id,
            claims.project_id,
            claims.agent_id,
            claims.agent_version,
            credentialId,
            field,
            version,
            delivery,
            claims.jti,
            new Date(claims.exp * 1000),
            this.d.now(),
          ],
        );
        try {
          await checkRunnerPolicy(db, claims, await this.d.secrets.credential(credentialId, db), delivery);
          gatewayEpoch = await this.d.gateway.push({
            release_id: releaseId,
            step_run_id: claims.sub,
            claim_id: claims.claim_id,
            generation: claims.generation,
            runner_id: claims.runner_id,
            credential_id: credentialId,
            delivery,
            binding,
            material,
            expires_at: new Date(expiry).toISOString(),
          });
        } catch (e) {
          await db.query(`UPDATE releases SET status = 'failed', ended_at = $2, reason = $3 WHERE id = $1`, [
            releaseId,
            this.d.now(),
            'delivery_failed',
          ]);
          throw e;
        }
        pushed = true;
      };
      if (field === 'access_token') {
        if (!this.d.oauth) throw unavailable();
        await this.d.oauth.ensureAccess(credentialId, 'release');
        await this.d.core.grant(claims.sub, {
          kind: 'capability', claim_id: claims.claim_id, generation: claims.generation, runner_id: claims.runner_id,
        }, { claim_id: claims.claim_id, generation: claims.generation, runner_id: claims.runner_id,
          group_id: claims.group_id, agent_id: claims.agent_id, agent_version: claims.agent_version, project_id: claims.project_id });
        await this.d.oauth.withAccess(credentialId, materialExpiry, async (access, db) => {
          if (access.generation !== version) throw unavailable();
          await deliver(access.material, access.binding, access.expiresAt, db);
        });
      } else {
        const opened = await this.d.secrets.openForRelease({
          credentialId, field, pinnedVersion: version,
          authorize: (credential) => checkRunnerPolicy(this.d.pool, claims, credential, delivery),
        });
        try {
          if (!groupAllowed(opened.groups, claims.group_id)) throw notAllowed();
          await deliver({ fields: { [field]: opened.value.toString('utf8') } }, opened.binding, materialExpiry, this.d.pool);
        } finally {
          opened.value.fill(0);
        }
      }
      await this.audit('credential.released', credentialId, cap, {
        release_id: releaseId,
        step_run_id: claims.sub,
        version,
        delivery,
        agent_id: claims.agent_id,
        agent_version: claims.agent_version,
      });
    } catch (e) {
      const reason = e instanceof VaultError && e.code === 'release.gateway_unavailable' ? 'delivery_failed' : e instanceof VaultError ? e.code : 'internal';
      if (pushed) {
        await this.d.gateway.remove(releaseId).catch((err: unknown) =>
          this.d.log.error('could not delete material at the gateway', { release_id: releaseId, ...errorFields(err) }),
        );
        await this.d.pool
          .query(`UPDATE releases SET status = 'failed', ended_at = $2, reason = $3 WHERE id = $1`, [releaseId, this.d.now(), reason])
          .catch((err: unknown) => this.d.log.error('could not mark the release failed', { release_id: releaseId, ...errorFields(err) }));
      }
      await this.d.core.revoked(claims.sub, credentialId, reason).catch((err: unknown) =>
        this.d.log.error('could not record the revocation in the core', { step_run_id: claims.sub, ...errorFields(err) }),
      );
      if (reason !== AUDIT_UNAVAILABLE) {
        await this.audit('credential.release_failed', credentialId, cap, { step_run_id: claims.sub, version, delivery, reason });
      }
      throw e;
    }

    this.renewer.track({
      releaseId,
      stepRunId: claims.sub,
      credentialId,
      credentialVersion: version,
      field,
      delivery,
      claims: {
        claim_id: claims.claim_id,
        generation: claims.generation,
        runner_id: claims.runner_id,
        group_id: claims.group_id,
        agent_id: claims.agent_id,
        agent_version: claims.agent_version,
        project_id: claims.project_id,
      },
      issuedAt: claims.iat,
      expiresAt: claims.exp,
      materialExpiry,
      gatewayEpoch,
    });
    this.d.log.info('released', { release_id: releaseId, step_run_id: claims.sub, credential_id: credentialId, version, delivery });
    return { release_id: releaseId, material_handle: releaseId, expires_at: new Date(materialExpiry).toISOString() };
  }

  async discover(body: Partial<McpDiscoveryRequest> | undefined, oauthAuthorized = false): Promise<{ tools: unknown[] }> {
    if (typeof body?.url !== 'string' || (body.transport !== 'streamable-http' && body.transport !== 'sse')) bad('endpoint');
    const { url, transport, credential_id } = body;
    if (credential_id === undefined) return this.d.gateway.discover({ url, transport });
    if (typeof credential_id !== 'string' || !ID.test(credential_id)) bad('credential_id');
    const credential = await this.d.secrets.credential(credential_id);
    if ((credential.type !== 'mcp-bearer' && !(credential.type === 'mcp-oauth' && oauthAuthorized)) || credential.status !== 'active' || this.d.secrets.binding(credential).binding.url !== url) {
      throw new VaultError(403, 'mcp.discovery_denied', 'MCP discovery credential unavailable');
    }
    return this.withTestMaterial(credential.id, async ({ releaseId, version }) => {
      const startedAt = this.d.now();
      const tools = await this.d.gateway.discover({ url, transport, release_id: releaseId });
      if (credential.type === 'mcp-oauth') {
        await this.d.oauth?.recordSuccessfulAccess(credential.id, version, startedAt);
      }
      return tools;
    }, oauthAuthorized);
  }

  private async withTestMaterial<T>(credentialId: string, action: (input: {
    releaseId: string; binding: Record<string, unknown>; version: number;
  }) => Promise<T>, refresh = false): Promise<T> {
    const credential = await this.d.secrets.credential(credentialId);
    const oauth = credential.type === 'mcp-oauth';
    const releaseId = id('rel_test');
    const stepRunId = `test:${releaseId}`;
    const delivery = typeImpl(credential.type).deliveries[0] as Delivery;
    const expires = this.d.now().getTime() + 1000 * TEST_RELEASE_SECONDS;
    const push = async (field: string, version: number, binding: Record<string, unknown>, material: OAuthAccess['material'], expiry: number,
      db: pg.Pool | pg.PoolClient) => {
      await db.query(
        `INSERT INTO releases (id, step_run_id, credential_id, field, credential_version, delivery, status, expires_at, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, 'test', $7, $8)`,
        [releaseId, stepRunId, credentialId, field, version, delivery, new Date(expiry), this.d.now()],
      );
      await this.d.gateway.push({
        release_id: releaseId, step_run_id: stepRunId, credential_id: credentialId, delivery, binding, material, expires_at: new Date(expiry).toISOString(),
      });
      return { releaseId, binding, version };
    };
    try {
      if (oauth) {
        if (!this.d.oauth) throw unavailable();
        if (refresh) await this.d.oauth.ensureAccess(credentialId, 'test');
        const access = await this.d.oauth.withAccess(credentialId, expires, (access, db) =>
          push('access_token', access.version, access.binding, access.material, access.expiresAt, db));
        return await action(access);
      }
      const opened = await this.d.secrets.openForRelease({ credentialId });
      try {
        return await action(await push(opened.field, opened.version, opened.binding,
          { fields: { [opened.field]: opened.value.toString('utf8') } }, expires, this.d.pool));
      } finally {
        opened.value.fill(0);
      }
    } finally {
      await this.d.gateway.remove(releaseId).catch((e: unknown) =>
        this.d.log.error('could not delete test material at the gateway', { release_id: releaseId, ...errorFields(e) }),
      );
      await this.d.pool.query('UPDATE releases SET ended_at = COALESCE(ended_at, $2) WHERE id = $1', [releaseId, this.d.now()]);
    }
  }

  async test(body: Partial<TestRequest>, actor: { kind: 'user'; id: string }): Promise<TestResponse> {
    if (typeof body?.credential_id !== 'string' || !ID.test(body.credential_id)) bad('credential_id');
    const credential = await this.d.secrets.credential(body.credential_id);
    const impl = typeImpl(credential.type);
    return this.withTestMaterial(credential.id, async ({ releaseId, binding, version }) => {
      const startedAt = this.d.now();
      const outcome = await this.d.gateway.test({ release_id: releaseId, request: impl.testRequest(binding, credential.config) });
      const result = classifyTest(outcome);
      if (result.ok && credential.type === 'mcp-oauth') {
        await this.d.oauth?.recordSuccessfulAccess(credential.id, version, startedAt);
      }
      await this.d.pool.query('UPDATE releases SET reason = $2 WHERE id = $1', [releaseId, result.ok ? 'ok' : result.error_class]);
      await audit(this.d.pool, {
        actor,
        action: 'credential.tested',
        resource_type: 'credential',
        resource_id: credential.id,
        group_id: null,
        runner_id: null,
        detail: { version, ok: result.ok, ...(result.error_class ? { error_class: result.error_class } : {}) },
      });
      this.d.log.info('credential tested', { credential_id: credential.id, ok: result.ok, error_class: result.error_class ?? null });
      return result;
    }, true);
  }
}
