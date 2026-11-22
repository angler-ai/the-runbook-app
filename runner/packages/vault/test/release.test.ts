// Capabilities, lease-bound releases and renewal (design §7.5), against a
// fake core (grant routes and JWKS) and a fake gateway (material routes).
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { FakeCore } from '@runbook/runner-shared/test-utils/fake-core';
import { FakeGateway } from '@runbook/runner-shared/test-utils/fake-gateway';
import { hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { sealSecret } from '@runbook/runner-shared/seal-client';
import { Renewer } from '../src/renewer.ts';
import {
  GROUP,
  type Harness,
  insertCredential,
  ORG,
  RUNNER,
  startHarness,
  TOKENS,
} from './helpers.ts';

const SECRET = 'fake-dd-key-release-CCCCCCCCCCCCCCCCCCCC';
const SECRET_V2 = 'fake-dd-key-rotated-DDDDDDDDDDDDDDDDDDDD';
const SR = 'sr_compare_error_rate';
const PROJECT = 'pr_ops';
const AGENT = 'ag_checker';
const VERSION = 'av_1';

describe.skipIf(!hasTestDatabase)('vault releases', () => {
  let h: Harness;
  let core: FakeCore;
  let gateway: FakeGateway;
  let credential: { id: string; hash: Buffer };

  beforeEach(async () => {
    const clock = { now: Date.now() };
    core = await new FakeCore({ now: () => clock.now, releaseToken: TOKENS.release }).start();
    gateway = await new FakeGateway(TOKENS.gateway).start();
    h = await startHarness({
      clock,
      core: { baseUrl: core.url, releaseToken: TOKENS.release },
      gateway: { baseUrl: gateway.url, token: TOKENS.gateway },
    });
    credential = await insertCredential(h, { name: 'datadog-readonly' });
    await seal(SECRET, 0);
    await h.db.admin().query(
      `INSERT INTO agents (id, project_id, group_id, name, status, current_version_id) VALUES ($1, $2, $3, 'checker', 'active', $4)`,
      [AGENT, PROJECT, GROUP, VERSION],
    );
    await h.db.admin().query(
      `INSERT INTO agent_versions (id, agent_id, version, config, ceiling, created_by) VALUES ($1, $2, 1, $3, '{}', 'us_admin')`,
      [VERSION, AGENT, JSON.stringify({ credentials: [{ credential_id: credential.id, delivery: 'proxy' }] })],
    );
  });
  afterEach(async () => {
    await h?.close();
    await core?.close();
    await gateway?.close();
  });

  async function seal(value: string, expectedVersion: number) {
    const key = (await h.call('GET', '/internal/ingest-key')).json;
    const blob = await sealSecret(key, {
      orgId: ORG,
      credentialId: credential.id,
      field: 'key',
      expectedVersion,
      bindingHash: credential.hash,
      value,
      now: h.clock.now,
    });
    const res = await h.call('POST', '/internal/seal', { blob });
    expect(res.status).toBe(200);
  }

  async function capability(sr = SR) {
    const lease = core.leases.get(sr) ?? core.lease(sr, { credentials: [credential.id] });
    const res = await h.call(
      'POST',
      '/internal/capability',
      { step_run_id: sr, claim_id: lease.claim_id, generation: lease.generation, runner_id: RUNNER },
      TOKENS.worker,
    );
    return res;
  }

  const release = (cap: string, credentialId = credential.id, delivery = 'proxy') =>
    h.call('POST', '/internal/release', { capability: cap, credential_id: credentialId, delivery }, TOKENS.worker);

  /** Hold the audit chain lock the way a stuck writer would: an INSERT in an open transaction. */
  async function holdAuditLock(): Promise<() => Promise<void>> {
    const holder = await h.db.admin().connect();
    await holder.query('BEGIN');
    await holder.query(`INSERT INTO audit_log (id, actor, action, resource_type) VALUES ('au_hold', '{}', 'hold', 'test')`);
    return async () => {
      await holder.query('ROLLBACK').catch(() => undefined);
      holder.release();
    };
  }

  function assertNoSecret(text: string) {
    for (const s of [SECRET, SECRET_V2]) {
      expect(text).not.toContain(s);
      expect(text).not.toContain(Buffer.from(s).toString('base64'));
    }
  }

  it('issues a capability from the core and releases material to the gateway', async () => {
    const cap = await capability();
    expect(cap.status).toBe(200);
    expect(Object.keys(cap.json).sort()).toEqual(['capability', 'expires_at']);
    const res = await release(cap.json.capability);
    expect(res.status).toBe(200);
    const push = gateway.material.get(res.json.release_id);
    expect(push).toMatchObject({
      step_run_id: SR,
      credential_id: credential.id,
      delivery: 'proxy',
      material: { fields: { key: SECRET } },
      binding: { hosts: ['api.datadoghq.com'], header_name: 'dd-api-key', header_format: '{}', groups: 'all' },
    });
    expect(Date.parse(res.json.expires_at)).toBe(core.leases.get(SR)!.deadline * 1000);
    const grants = core.grantRequests('credential');
    expect(grants).toHaveLength(1);
    expect(grants[0]?.body).toMatchObject({ credential_id: credential.id, credential_version: 1, delivery: 'proxy' });
    const { rows } = await h.db.admin().query(`SELECT status, credential_version, delivery, group_id, runner_id FROM releases`);
    expect(rows).toEqual([{ status: 'live', credential_version: 1, delivery: 'proxy', group_id: GROUP, runner_id: RUNNER }]);
    const audit = await h.db.admin().query(`SELECT action FROM audit_log ORDER BY seq`);
    expect(audit.rows.map((r) => r.action)).toContain('credential.released');
  });

  it('releases only the signed profile model key when connections is empty', async () => {
    const endpoint = 'https://model.example';
    const model = await insertCredential(h, { type: 'model-provider', config: { provider: 'anthropic', endpoint } });
    const other = await insertCredential(h, { type: 'model-provider', config: { provider: 'anthropic', endpoint } });
    const ingest = (await h.call('GET', '/internal/ingest-key')).json;
    const blob = await sealSecret(ingest, { orgId: ORG, credentialId: model.id, field: 'api_key',
      expectedVersion: 0, bindingHash: model.hash, value: SECRET_V2, now: h.clock.now });
    expect((await h.call('POST', '/internal/seal', { blob })).status).toBe(200);
    await h.db.admin().query(
      `INSERT INTO providers(id,name,type,auth,credential_id,endpoint,sharing)
       VALUES ('main','main','anthropic','stored',$1,$3,'{"projects":"all"}'),
              ('other','other','anthropic','stored',$2,$3,'{"projects":"all"}')`, [model.id, other.id, endpoint],
    );
    await h.db.admin().query(`INSERT INTO profiles(id,name,provider_id,model,sharing)
      VALUES ('fast','fast','main','test-model','{"projects":"all"}'),
             ('deep','deep','other','test-model','{"projects":"all"}')`);
    await h.db.admin().query(`UPDATE agent_versions SET config = config || '{"profiles":["fast","deep"]}' WHERE id = $1`, [VERSION]);
    core.lease(SR, { credentials: [], model_profile: 'fast' });
    const cap = await capability();
    expect(cap.status).toBe(200);
    expect((await release(cap.json.capability, other.id, 'model')).status).toBe(403);
    expect((await release(cap.json.capability)).status).toBe(403);
    expect((await release(cap.json.capability, model.id, 'model')).status).toBe(200);
    expect(gateway.pushes).toEqual([expect.objectContaining({
      credential_id: model.id, delivery: 'model', material: { fields: { api_key: SECRET_V2 } },
    })]);

    const decision = core.lease(SR, { reach: 'read', network: [], credentials: [], model_profile: 'fast' });
    const excluded = await capability();
    expect((await release(excluded.json.capability, model.id, 'model')).status).toBe(403);
    decision.credentials = [model.id];
    const read = await capability();
    expect((await release(read.json.capability, model.id, 'model')).status).toBe(200);
    expect((await h.call('POST', `/internal/credential-metadata/${model.id}`, { label: 'write' }, TOKENS.api)).status).toBe(200);
    expect((await release(read.json.capability, model.id, 'model')).status).toBe(403);
  });

  it('refuses token delivery before requesting a release grant', async () => {
    const cap = await capability();
    const res = await release(cap.json.capability, credential.id, 'token');
    expect(res.status).toBe(403);
    expect(res.json.error.code).toBe('release.token_delivery_disabled');
    expect(core.grantRequests('credential')).toHaveLength(0);
  });

  it('drops material immediately when revoked and destroys wrapped keys on delete', async () => {
    const cap = await capability();
    const live = await release(cap.json.capability);
    expect(live.status).toBe(200);
    expect(gateway.material.has(live.json.release_id)).toBe(true);
    const body = { credential_id: credential.id, destroy: false };
    expect((await h.call('POST', '/internal/revoke', body, TOKENS.worker)).status).toBe(403);
    expect((await h.call('POST', '/internal/revoke', body, TOKENS.api, false)).status).toBe(401);
    expect((await h.call('POST', '/internal/revoke', body, TOKENS.api, { org_role: 'member' })).status).toBe(403);
    expect(gateway.material.has(live.json.release_id)).toBe(true);
    expect((await h.call('POST', '/internal/revoke', body, TOKENS.api)).status).toBe(204);
    expect(gateway.material.has(live.json.release_id)).toBe(false);
    expect((await release(cap.json.capability)).status).toBe(403);
    expect((await h.db.admin().query(
      'SELECT wrapped_dek IS NULL AS destroyed FROM credential_secret_versions WHERE credential_id = $1', [credential.id],
    )).rows).toEqual([{ destroyed: false }]);
    expect((await h.call('POST', '/internal/revoke', { ...body, destroy: true }, TOKENS.api)).status).toBe(204);
    expect((await h.db.admin().query(
      'SELECT wrapped_dek IS NULL AS destroyed FROM credential_secret_versions WHERE credential_id = $1', [credential.id],
    )).rows).toEqual([{ destroyed: true }]);
  });

  it('creates or updates credential label and sharing together and reuses a successful create on retry', async () => {
    const path = '/internal/credential-metadata';
    const input = { type: 'model-provider', name: 'atomically initialized model', label: 'destructive',
      config: { provider: 'anthropic', endpoint: 'https://api.example' }, sharing: { projects: ['project'] } };
    const first = await h.call('POST', path, input, TOKENS.api);
    expect(first.status).toBe(200);
    expect(first.json).toMatchObject({ label: 'destructive', sharing: { projects: ['project'] }, status: 'needs_secret' });
    const retry = await h.call('POST', path, input, TOKENS.api);
    expect(retry.json.id).toBe(first.json.id);
    const id = first.json.id;
    const state = async () => (await h.db.admin().query(
      'SELECT label, sharing, label_initialized FROM credentials WHERE id = $1', [id])).rows[0];
    const update = `${path}/${id}`;
    expect((await h.call('POST', update, { label: 'invalid', sharing: { projects: 'all' } }, TOKENS.api)).status).toBe(400);
    expect(await state()).toEqual({ label: 'destructive', sharing: { projects: ['project'] }, label_initialized: true });
    expect((await h.call('POST', update, { name: 'new display name' }, TOKENS.api)).status).toBe(200);
    expect(await state()).toEqual({ label: 'destructive', sharing: { projects: ['project'] }, label_initialized: true });
    expect((await h.call('POST', update, { label: 'read', sharing: { projects: 'all' } }, TOKENS.api)).status).toBe(200);
    expect(await state()).toEqual({ label: 'read', sharing: { projects: 'all' }, label_initialized: true });
  });

  it('requires an admin assertion for relabeling and checks the vault label at release', async () => {
    const path = `/internal/credential-metadata/${credential.id}`;
    const body = { label: 'destructive' };
    expect((await h.call('POST', path, body, TOKENS.api, false)).status).toBe(401);
    expect((await h.call('POST', path, body, TOKENS.api, { org_role: 'member' })).status).toBe(403);
    expect((await h.call('POST', path, body, TOKENS.api, { htu: '/runner/v1/credentials/other' })).status).toBe(401);
    expect((await h.call('POST', path, body, TOKENS.api)).status).toBe(200);
    const cap = await capability();
    expect((await release(cap.json.capability)).status).toBe(403);
    expect((await h.db.admin().query("SELECT actor FROM audit_log WHERE action = 'credential.label_changed'")).rows)
      .toEqual([{ actor: { kind: 'user', id: 'us_admin' } }]);
  });

  it('rejects a stale signed read set after the vault relabels a credential', async () => {
    const lease = core.lease(SR, { reach: 'read', network: [], credentials: [credential.id] });
    const cap = await capability();
    const live = await release(cap.json.capability);
    expect(live.status).toBe(200);

    const path = `/internal/credential-metadata/${credential.id}`;
    expect((await h.call('POST', path, { label: 'write' }, TOKENS.api)).status).toBe(200);
    const rejected = await release(cap.json.capability);
    expect(rejected.status).toBe(403);
    expect(rejected.json.error.code).toBe('release.not_allowed');

    h.clock.now += 430_000;
    lease.deadline = Math.floor(h.clock.now / 1000) + 600;
    await h.vault.releases!.renewer.runDue();
    expect(gateway.pushes).toHaveLength(1);
    expect(gateway.material.has(live.json.release_id)).toBe(false);
  });

  it('material_never_in_response_body', async () => {
    const cap = await capability();
    const res = await release(cap.json.capability);
    expect(Object.keys(res.json).sort()).toEqual(['expires_at', 'material_handle', 'release_id']);
    assertNoSecret(res.text);
    assertNoSecret(cap.text);
    assertNoSecret(h.logs.text());
    const audit = await h.db.admin().query(`SELECT detail::text AS d FROM audit_log`);
    assertNoSecret(JSON.stringify(audit.rows));
    // The secret did reach the gateway, and only the gateway.
    expect(JSON.stringify(gateway.pushes)).toContain(SECRET);
  });

  it('release_requires_valid_capability', async () => {
    for (const bad of ['', 'not.a.jwt', 'eyJhbGciOiJub25lIn0.e30.']) {
      const res = await release(bad);
      expect(res.status, bad).toBe(401);
      expect(res.json.error.code).toBe('release.invalid_capability');
    }
    // A capability past its lease deadline (plus skew) is refused.
    const cap = await capability();
    h.clock.now += 11 * 60_000;
    const late = await release(cap.json.capability);
    expect(late.status).toBe(401);
    expect(late.json.error.code).toBe('release.invalid_capability');
    h.clock.now -= 11 * 60_000;
    // Only the worker may ask for releases.
    const body = { capability: cap.json.capability, credential_id: credential.id, delivery: 'proxy' };
    expect((await h.call('POST', '/internal/release', body, TOKENS.api)).status).toBe(403);
    expect(core.grantRequests('credential')).toHaveLength(0);
    expect(gateway.pushes).toHaveLength(0);
    // The same capability, in time and from the worker, works.
    expect((await release(cap.json.capability)).status).toBe(200);
  });

  it('release_checks_runner_policy_before_core', async () => {
    const other = await insertCredential(h, { name: 'not-listed' });
    const cap = await capability();
    const before = core.grantRequests('credential').length;
    // Not in the agent version.
    const res = await release(cap.json.capability, other.id);
    expect(res.status).toBe(403);
    expect(res.json.error.code).toBe('release.not_allowed');
    // Listed, but with another delivery mode.
    expect((await release(cap.json.capability, credential.id, 'mcp')).json.error.code).toBe('release.not_allowed');
    // Listed, but no longer shared with the project.
    await h.db.admin().query(`UPDATE credentials SET sharing = '{"projects": ["pr_other"]}' WHERE id = $1`, [credential.id]);
    expect((await release(cap.json.capability)).json.error.code).toBe('release.not_allowed');
    await h.db.admin().query(`UPDATE credentials SET sharing = '{"projects": "none"}' WHERE id = $1`, [credential.id]);
    expect((await release(cap.json.capability)).json.error.code).toBe('release.not_allowed');
    // Revoked.
    await h.db.admin().query(`UPDATE credentials SET sharing = '{"projects": "all"}', status = 'revoked' WHERE id = $1`, [credential.id]);
    expect((await release(cap.json.capability)).status).toBe(403);
    // The core was never asked.
    expect(core.grantRequests('credential').length).toBe(before);
    expect(gateway.pushes).toHaveLength(0);
    const audit = await h.db.admin().query(`SELECT count(*)::int AS n FROM audit_log WHERE action = 'credential.release_denied'`);
    expect(audit.rows[0].n).toBeGreaterThan(0);
  });

  it('grant_signature_and_aud_verified', async () => {
    // A capability signed by a key that isn't in the core's JWKS.
    core.rogue = true;
    const rogueCap = await capability();
    expect(rogueCap.status).toBe(502);
    expect(rogueCap.json.error.code).toBe('release.invalid_grant');
    core.rogue = false;
    const cap = await capability();
    expect(cap.status).toBe(200);
    // A credential grant signed by an unknown key.
    core.rogue = true;
    const res = await release(cap.json.capability);
    expect(res.status).toBe(502);
    expect(res.json.error.code).toBe('release.invalid_grant');
    core.rogue = false;
    // A grant for another runner's audience.
    core.audienceOverride = 'runner:rn_someone_else';
    expect((await release(cap.json.capability)).json.error.code).toBe('release.invalid_grant');
    core.audienceOverride = undefined;
    expect(gateway.pushes).toHaveLength(0);
    expect((await release(cap.json.capability)).status).toBe(200);
  });

  it('refuses a credential outside the signed step connections before requesting a grant', async () => {
    const lease = core.lease(SR, { credentials: [] });
    const cap = await h.call(
      'POST',
      '/internal/capability',
      { step_run_id: SR, claim_id: lease.claim_id, generation: lease.generation, runner_id: RUNNER },
      TOKENS.worker,
    );
    const res = await release(cap.json.capability);
    expect(res.status).toBe(403);
    expect(res.json.error.code).toBe('release.not_allowed');
    expect(core.grantRequests('credential')).toHaveLength(0);
    expect(gateway.pushes).toHaveLength(0);
  });

  it('renews at 70% of the grant lifetime and extends the material', async () => {
    const cap = await capability();
    const res = await release(cap.json.capability);
    const lease = core.leases.get(SR)!;
    h.clock.now += 300_000;
    await h.vault.releases!.renewer.runDue();
    expect(gateway.pushes).toHaveLength(1); // 50%: not yet.
    h.clock.now += 130_000;
    lease.deadline = Math.floor(h.clock.now / 1000) + 600; // the worker heartbeated
    await h.vault.releases!.renewer.runDue();
    expect(gateway.pushes).toHaveLength(2);
    expect(gateway.pushes[1]).toMatchObject({ release_id: res.json.release_id });
    expect(Date.parse(gateway.pushes[1]!.expires_at)).toBe(lease.deadline * 1000);
    const { rows } = await h.db.admin().query(`SELECT status, expires_at FROM releases`);
    expect(rows[0].status).toBe('live');
    expect(rows[0].expires_at.getTime()).toBe(lease.deadline * 1000);
  });

  it('refuses a capability request naming a different runner than the worker token', async () => {
    const lease = core.lease(SR, { runner_id: 'rn_prod', group_id: 'rg_prod_write', credentials: [credential.id] });
    const res = await h.call('POST', '/internal/capability', {
      step_run_id: SR, claim_id: lease.claim_id, generation: lease.generation, runner_id: 'rn_prod',
    }, TOKENS.worker);
    expect(res.status).toBe(403);
    expect(core.grantRequests('capability')).toHaveLength(0);
  });

  it('lets the worker fence a claim before sandbox creation', async () => {
    const cap = await capability();
    const first = await release(cap.json.capability);
    expect(gateway.material.has(first.json.release_id)).toBe(true);
    expect((await h.call('POST', '/internal/end-claim', { capability: cap.json.capability }, TOKENS.worker)).status).toBe(204);
    expect(gateway.material.size).toBe(0);
    expect(core.revoked.at(-1)).toMatchObject({ reason: 'claim.ended' });
  });

  it('backs off repeated liveness probes for a live claim', async () => {
    await release((await capability()).json.capability);
    const renewer = h.vault.releases!.renewer;
    await renewer.runDue();
    const initial = core.grantRequests('capability').length;
    h.clock.now += 5_000;
    await renewer.runDue();
    expect(core.grantRequests('capability')).toHaveLength(initial);
    h.clock.now += 5_000;
    await renewer.runDue();
    expect(core.grantRequests('capability')).toHaveLength(initial + 1);
  });

  it('fences an externally ended claim on the next sweep even without a sandbox', async () => {
    const cap = await capability();
    const res = await release(cap.json.capability);
    expect(gateway.material.has(res.json.release_id)).toBe(true);
    core.takeOver(SR);
    h.clock.now += 5_000; // within the first scheduled sweep
    await h.vault.releases!.renewer.runDue();
    expect(gateway.deletes).toEqual([res.json.release_id]);
    expect(gateway.material.has(res.json.release_id)).toBe(false);
    expect(core.revoked).toEqual([{ step_run_id: SR, credential_id: credential.id, reason: 'grant.claim_moved' }]);
    const { rows } = await h.db.admin().query(`SELECT status, reason FROM releases`);
    expect(rows).toEqual([{ status: 'revoked', reason: 'grant.claim_moved' }]);
    // Nothing more is renewed or pushed for it.
    h.clock.now += 600_000;
    await h.vault.releases!.renewer.runDue();
    expect(gateway.pushes).toHaveLength(1);
    expect(core.grantRequests('credential')).toHaveLength(1);
    // And a new capability for the moved claim is refused.
    const again = await h.call(
      'POST',
      '/internal/capability',
      { step_run_id: SR, claim_id: core.leases.get(SR)!.claim_id, generation: 1, runner_id: RUNNER },
      TOKENS.worker,
    );
    expect(again.status).toBe(409);
    expect(again.json.error.code).toBe('grant.claim_moved');
  });

  it('revoking a credential ends its live releases at the next pass', async () => {
    const res = await release((await capability()).json.capability);
    await h.db.admin().query(`UPDATE credentials SET status = 'revoked' WHERE id = $1`, [credential.id]);
    await h.vault.releases!.renewer.runDue();
    expect(gateway.deletes).toEqual([res.json.release_id]);
    expect(core.revoked).toEqual([{ step_run_id: SR, credential_id: credential.id, reason: 'credential.revoked' }]);
    expect(h.vault.releases!.renewer.size).toBe(0);
  });

  it('a restarted vault picks up live releases and renews them', async () => {
    const res = await release((await capability()).json.capability);
    const fresh = new Renewer(h.vault.releases!.d);
    expect(await fresh.loadLive()).toBe(1);
    await fresh.runDue();
    expect(gateway.pushes.map((p) => p.release_id)).toEqual([res.json.release_id, res.json.release_id]);
  });

  it('a rotated secret keeps the pinned version for the live grant; new grants get the new one', async () => {
    const cap = await capability();
    const first = await release(cap.json.capability);
    await seal(SECRET_V2, 1);
    h.clock.now += 430_000;
    core.leases.get(SR)!.deadline = Math.floor(h.clock.now / 1000) + 600;
    await h.vault.releases!.renewer.runDue();
    const renewal = gateway.pushes.at(-1)!;
    expect(renewal.release_id).toBe(first.json.release_id);
    expect(renewal.material.fields.key).toBe(SECRET);
    const second = await release((await capability()).json.capability);
    expect(gateway.material.get(second.json.release_id)?.material.fields.key).toBe(SECRET_V2);
    // After the 24-hour grace the old version can't be renewed: the release ends.
    h.clock.now += 24 * 3600_000;
    core.leases.get(SR)!.deadline = Math.floor(h.clock.now / 1000) + 600;
    await h.vault.releases!.renewer.runDue();
    expect(gateway.deletes).toContain(first.json.release_id);
    expect(core.revoked.at(-1)).toMatchObject({ credential_id: credential.id, reason: 'secret.version_expired' });
    assertNoSecret(h.logs.text());
  });

  it('test_connection_returns_fixed_error_class', async () => {
    // An upstream that rejects the key and echoes it back in the error.
    const seen: string[] = [];
    const upstream = http.createServer((req, res) => {
      const key = String(req.headers['dd-api-key'] ?? '');
      seen.push(key);
      res.writeHead(key === SECRET ? 401 : 500, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ errors: [`API key ${key} is invalid`] }));
    });
    await new Promise<void>((resolve) => upstream.listen(0, '127.0.0.1', resolve));
    gateway.upstream = `http://127.0.0.1:${(upstream.address() as AddressInfo).port}`;
    try {
      const body = { credential_id: credential.id };
      expect((await h.call('POST', '/internal/test', body, TOKENS.api, false)).status).toBe(401);
      expect((await h.call('POST', '/internal/test', body, TOKENS.api, { org_role: 'member' })).status).toBe(403);
      expect(seen).toEqual([]);
      const res = await h.call('POST', '/internal/test', body, TOKENS.api, { user_id: 'us_operator' });
      expect(res.status).toBe(200);
      expect(res.json).toEqual({ ok: false, error_class: 'unauthorized' });
      assertNoSecret(res.text);
      // The test release went to the gateway, used the real key, and was removed.
      expect(seen).toEqual([SECRET]);
      const testPush = gateway.pushes.at(-1)!;
      expect(gateway.deletes).toContain(testPush.release_id);
      expect(gateway.material.size).toBe(0);
      // Other classes come only from the fixed list.
      gateway.testOutcome = 'tls';
      expect((await h.call('POST', '/internal/test', { credential_id: credential.id })).json).toEqual({
        ok: false,
        error_class: 'tls',
      });
      gateway.testOutcome = 'unreachable';
      expect((await h.call('POST', '/internal/test', { credential_id: credential.id })).json).toEqual({
        ok: false,
        error_class: 'unreachable',
      });
      // Only runner-api may run tests.
      expect((await h.call('POST', '/internal/test', { credential_id: credential.id }, TOKENS.worker)).status).toBe(403);
      assertNoSecret(h.logs.text());
      const audit = await h.db.admin().query("SELECT actor, db_role, detail::text AS d FROM audit_log WHERE action = 'credential.tested' ORDER BY seq");
      expect(audit.rows.length).toBe(3);
      expect(audit.rows[0]).toMatchObject({ actor: { kind: 'user', id: 'us_operator' }, db_role: 'rb_vault_test_login' });
      assertNoSecret(JSON.stringify(audit.rows));
    } finally {
      upstream.closeAllConnections();
      await new Promise<void>((resolve) => upstream.close(() => resolve()));
    }
  });

  it('a successful test reports ok', async () => {
    const upstream = http.createServer((_req, res) => {
      res.writeHead(200);
      res.end('{"valid":true}');
    });
    await new Promise<void>((resolve) => upstream.listen(0, '127.0.0.1', resolve));
    gateway.upstream = `http://127.0.0.1:${(upstream.address() as AddressInfo).port}`;
    try {
      expect((await h.call('POST', '/internal/test', { credential_id: credential.id })).json).toEqual({ ok: true });
    } finally {
      upstream.closeAllConnections();
      await new Promise<void>((resolve) => upstream.close(() => resolve()));
    }
  });

  it('a failed push to the gateway fails the release and tells the core', async () => {
    gateway.failPushes = true;
    const cap = await capability();
    const res = await release(cap.json.capability);
    expect(res.status).toBe(502);
    expect(res.json.error.code).toBe('release.gateway_unavailable');
    expect(core.revoked).toEqual([{ step_run_id: SR, credential_id: credential.id, reason: 'delivery_failed' }]);
    const { rows } = await h.db.admin().query(`SELECT status FROM releases`);
    expect(rows).toEqual([{ status: 'failed' }]);
  });

  it('a release whose audit entry cannot be written fails with audit.unavailable and is withdrawn', async () => {
    const cap = await capability();
    const unlock = await holdAuditLock();
    try {
      const started = Date.now();
      const res = await release(cap.json.capability);
      expect(Date.now() - started).toBeLessThan(5_000);
      expect(res.status).toBe(503);
      expect(res.json.error.code).toBe('audit.unavailable');
      expect(gateway.material.size).toBe(0);
      expect(h.vault.releases!.renewer.size).toBe(0);
    } finally {
      await unlock();
    }
    const { rows } = await h.db.admin().query(`SELECT status, reason FROM releases`);
    expect(rows).toEqual([{ status: 'failed', reason: 'audit.unavailable' }]);
    expect(core.revoked).toEqual([{ step_run_id: SR, credential_id: credential.id, reason: 'audit.unavailable' }]);
  });

  it('an audit log that cannot be written does not stop the renewal pass', async () => {
    const first = (await release((await capability()).json.capability)).json.release_id;
    const second = (await release((await capability('sr_other')).json.capability)).json.release_id;
    core.takeOver(SR);
    h.clock.now += 430_000;
    core.leases.get('sr_other')!.deadline = Math.floor(h.clock.now / 1000) + 600;
    const unlock = await holdAuditLock();
    try {
      const started = Date.now();
      await h.vault.releases!.renewer.runDue();
      expect(Date.now() - started).toBeLessThan(5_000);
    } finally {
      await unlock();
    }
    // The taken-over release still ended (material gone, row revoked); the other was renewed.
    expect(gateway.deletes).toEqual([first]);
    expect(gateway.pushes.map((p) => p.release_id)).toEqual([first, second, second]);
    const { rows } = await h.db.admin().query(`SELECT id, status FROM releases ORDER BY created_at`);
    expect(rows.find((r) => r.id === first)?.status).toBe('revoked');
    expect(rows.find((r) => r.id === second)?.status).toBe('live');
    expect(h.logs.text()).toContain('audit.unavailable');
  });

  it('refuses grants that outlive a lease (10 minutes plus 30 s of skew)', async () => {
    const nowS = Math.floor(h.clock.now / 1000);
    const lease = core.lease(SR, { credentials: [credential.id], deadline: nowS + 3600 });
    const ask = () =>
      h.call(
        'POST',
        '/internal/capability',
        { step_run_id: SR, claim_id: lease.claim_id, generation: lease.generation, runner_id: RUNNER },
        TOKENS.worker,
      );
    const long = await ask();
    expect(long.status).toBe(502);
    expect(long.json.error.code).toBe('release.invalid_grant');
    lease.deadline = nowS + 631;
    expect((await ask()).status).toBe(502);
    lease.deadline = nowS + 630;
    const ok = await ask();
    expect(ok.status).toBe(200);
    // A credential grant that outlives the lease is refused too.
    lease.deadline = nowS + 3600;
    const res = await release(ok.json.capability);
    expect(res.status).toBe(502);
    expect(res.json.error.code).toBe('release.invalid_grant');
    expect(gateway.pushes).toHaveLength(0);
  });

  it('refuses releases and renewals for an agent that is not active', async () => {
    const cap = (await capability()).json.capability;
    for (const status of ['paused', 'draft', 'archived']) {
      await h.db.admin().query(`UPDATE agents SET status = $2 WHERE id = $1`, [AGENT, status]);
      const res = await release(cap);
      expect(res.status, status).toBe(403);
      expect(res.json.error.code, status).toBe('release.not_allowed');
    }
    expect(core.grantRequests('credential')).toHaveLength(0);
    // A live release ends at its next renewal once the agent is paused.
    await h.db.admin().query(`UPDATE agents SET status = 'active' WHERE id = $1`, [AGENT]);
    const live = await release(cap);
    expect(live.status).toBe(200);
    await h.db.admin().query(`UPDATE agents SET status = 'paused' WHERE id = $1`, [AGENT]);
    h.clock.now += 430_000;
    core.leases.get(SR)!.deadline = Math.floor(h.clock.now / 1000) + 600;
    await h.vault.releases!.renewer.runDue();
    expect(gateway.deletes).toEqual([live.json.release_id]);
    expect(core.revoked.at(-1)).toMatchObject({ credential_id: credential.id, reason: 'release.not_allowed' });
  });
});
