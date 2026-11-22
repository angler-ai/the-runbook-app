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
      return runnerReply();
    };
    const worker = await startWorker({
      ...transport === 'the certificate binding'
        ? {
            serviceBindings: { RUNBOOK_RUNNER_MTLS: runnerFetch },
            outboundService: () => new Response('Unexpected public fetch', { status: 502 }),
          }
        : { outboundService: runnerFetch },
      faults: false,
      d1: { DATA: { id: '00000000-0000-0000-0000-0000000000b2' } },
      vars: {
        RUNBOOK_D1_SHARDS: JSON.stringify({ control: 'DB', default: 'DATA' }),
        BOOTSTRAP_SECRET: boot,
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

    await runnerScenario({
      call, token, project, org, user, url: 'https://runner.example', forwarded,
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
