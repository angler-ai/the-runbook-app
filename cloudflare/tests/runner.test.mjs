// The production router and storage adapters under local workerd.
import { createHash, randomBytes } from 'node:crypto';
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { startWorker } from './harness.mjs';
import { runnerClient, runnerScenario } from '../../crates/runbook-worker-conformance/cases/runner.mjs';

for (const transport of ['public fetch', 'the certificate binding']) {
  test(`a runner discovers, claims, obtains grants for and reports a Cloudflare step with ${transport}`, async (t) => {
    const boot = randomBytes(24).toString('hex');
    const forwarded = [];
    let runnerReply = () => Response.json({ groups: [] }, { headers: { 'set-cookie': 'runner=secret' } });
    const runnerFetch = async (request) => {
      const url = new URL(request.url);
      assert.equal(url.origin, 'https://runner.example');
      forwarded.push({ url, method: request.method, headers: request.headers, body: await request.text() });
      const reply = await runnerReply();
      return typeof reply === 'function' ? reply() : reply;
    };
    const auth = (request) => {
      if (new URL(request.url).pathname !== '/internal/session') return new Response(null, { status: 404 });
      const cookie = request.headers.get('cookie');
      if (cookie === '__Host-runbook.session_token=oauth-unavailable') return new Response(null, { status: 503 });
      const session = /^__Host-runbook\.session_token=(oauth-current|oauth-other)$/.exec(cookie ?? '')?.[1];
      if (!session) return new Response(null, { status: 401 });
      return Response.json({ auth_user_id: 'fixture-oauth-owner', email: 'runner-owner@example.test', email_verified: true,
        session_id: session, expires_at: Date.now() + 3600_000, mfa_enrolled: true,
        ...(session === 'oauth-current' && { mfa_method: 'totp', mfa_verified_at: Date.now() }) });
    };
    const worker = await startWorker({
      ...transport === 'the certificate binding'
        ? {
            serviceBindings: { RUNBOOK_RUNNER_MTLS: runnerFetch, AUTH: auth },
            outboundService: () => new Response('Unexpected public fetch', { status: 502 }),
          }
        : { outboundService: runnerFetch, serviceBindings: { AUTH: auth } },
      faults: false,
      d1: { DATA: { id: '00000000-0000-0000-0000-0000000000b2' } },
      vars: {
        RUNBOOK_D1_SHARDS: JSON.stringify({ control: 'DB', default: 'DATA' }),
        BOOTSTRAP_SECRET: boot,
        RUNBOOK_ORIGIN: 'https://runbook.test',
        RUNBOOK_AUTH_INTERNAL_SECRET: 'fake-cloudflare-session-fixture-0123456789',
        WEBHOOK_SECRET_KEY: randomBytes(32).toString('base64'),
      },
    });
    t.after(() => worker.dispose());
    const db = await worker.mf.getD1Database('DB');
    const call = runnerClient(worker.fetch);
    const { body: owner } = await call('POST', '/internal/bootstrap', 201, {
      body: { organization: 'Runner test', user_name: 'Owner', project_slug: 'ops' },
      headers: { 'x-bootstrap-secret': boot },
    });
    const { personal_token: token, project_id: project, org_id: org, user_id: user } = owner;
    const hash = [...createHash('sha256').update(token).digest()];
    await db.prepare(
      "UPDATE credential SET payload = json_insert(payload, '$.scopes[#]', 'runner:read', '$.scopes[#]', 'runner:admin') WHERE secret_hash = ?",
    ).bind(hash).run();

    await db.prepare('INSERT INTO verified_email(email, user_id) VALUES (?, ?)').bind('runner-owner@example.test', user).run();

    await runnerScenario({
      call, token, project, org, user, url: 'https://runner.example', forwarded, oauthBrowser: { origin: 'https://runbook.test' },
      readiness: '/health',
      setRunnerReply: (reply) => { runnerReply = reply; },
    });
    const audit = await db.prepare(
      'SELECT action, actor FROM runner_vault_identity_audit WHERE org_id = ? ORDER BY sequence',
    ).bind(org).all();
    assert.deepEqual(audit.results, [
      { action: 'runner.vault_identity_enrolled', actor: 'installer bootstrap' },
      { action: 'runner.vault_identity_changed', actor: user },
    ]);
  });
}

test('JWKS is empty without a sealing key', async (t) => {
  const worker = await startWorker({ faults: false });
  t.after(() => worker.dispose());
  const { body } = await runnerClient(worker.fetch)('GET', '/.well-known/jwks.json', 200);
  assert.deepEqual(body, { keys: [] });
});
