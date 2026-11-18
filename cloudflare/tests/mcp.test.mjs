// CF7: `/mcp` on the Cloudflare edition, routed like a deployed site's.
// The api Worker answers one JSON-RPC message per POST with the portable
// handler (runbook_api::mcp): the same prechecks as natively, a bearer only,
// and each tool call answered by the router in process, which reaches a
// run's ExecutionDO over the object protocol. LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { startWorker } from './harness.mjs';

const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');
const ORIGIN = 'https://runbook.test';
let worker;
let pat;
let project;
before(async () => {
  worker = await startWorker({ faults: false, vars: { BOOTSTRAP_SECRET, ORIGIN } });
  const boot = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops' }),
  });
  assert.equal(boot.status, 201, await boot.clone().text());
  ({ personal_token: pat, project_id: project } = await boot.json());
  const source = [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata: {id: verify, title: Verify}',
    'spec: {steps: [{id: check, title: Check}]}',
    '',
  ].join('\n');
  const headers = { authorization: `Bearer ${pat}`, 'content-type': 'application/json' };
  const created = await worker.fetch(`/v1/projects/${project}/runbooks`, {
    method: 'POST',
    headers: { ...headers, 'idempotency-key': 'create-1' },
    body: JSON.stringify({ source }),
  });
  assert.equal(created.status, 201, await created.clone().text());
  const rb = (await created.json()).runbook_id;
  const draft = await worker.fetch(`/v1/runbooks/${rb}/draft`, { headers });
  const published = await worker.fetch(`/v1/runbooks/${rb}/versions`, {
    method: 'POST',
    headers: { ...headers, 'idempotency-key': 'publish-1', 'if-match': draft.headers.get('etag') },
    body: '{}',
  });
  assert.equal(published.status, 201, await published.clone().text());
});
after(async () => {
  await worker?.dispose();
});

let seq = 0;
async function rpc(method, params, { bearer = pat, headers = {} } = {}) {
  seq += 1;
  const response = await worker.fetch('/mcp', {
    method: 'POST',
    headers: {
      ...(bearer ? { authorization: `Bearer ${bearer}` } : {}),
      accept: 'application/json',
      'content-type': 'application/json',
      ...headers,
    },
    body: JSON.stringify({ jsonrpc: '2.0', id: seq, method, params }),
  });
  const text = await response.text();
  let body = null;
  try {
    body = JSON.parse(text);
  } catch {}
  return { status: response.status, body, text };
}

test('initialize and tools/list answer over POST /mcp with a bearer', async () => {
  const init = await rpc('initialize', {
    protocolVersion: '2025-06-18',
    capabilities: {},
    clientInfo: { name: 'test', version: '0' },
  });
  assert.equal(init.status, 200, init.text);
  assert.equal(init.body.result.serverInfo.name, 'runbook', init.text);
  const tools = await rpc('tools/list', {});
  assert.equal(tools.status, 200, tools.text);
  assert.ok(tools.body.result.tools.some((t) => t.name === 'start_execution'), tools.text);
});

test('the native prechecks hold: no bearer, a foreign origin, GET', async () => {
  const anonymous = await rpc('tools/list', {}, { bearer: null });
  assert.equal(anonymous.status, 401, anonymous.text);
  const foreign = await rpc('tools/list', {}, { headers: { origin: 'https://evil.example' } });
  assert.equal(foreign.status, 403, foreign.text);
  const get = await worker.fetch('/mcp', { headers: { authorization: `Bearer ${pat}` } });
  assert.equal(get.status, 405);
});

test('a tool starts a run and reads it back through its ExecutionDO', async () => {
  const started = await rpc('tools/call', {
    name: 'start_execution',
    arguments: { project: 'ops', runbook: 'verify', idempotency_key: 'mcp-start-1' },
  });
  assert.equal(started.status, 200, started.text);
  assert.ok(!started.body.result.isError, started.text);
  const exe = started.text.match(/exe_[0-9A-Z]{26}/)?.[0];
  assert.ok(exe, started.text);
  const read = await rpc('tools/call', { name: 'get_execution', arguments: { execution_id: exe } });
  assert.equal(read.status, 200, read.text);
  assert.ok(!read.body.result.isError, read.text);
  assert.ok(read.text.includes(exe), read.text);
});
