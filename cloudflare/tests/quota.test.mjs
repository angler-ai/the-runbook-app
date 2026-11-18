// CF6: monthly execution quotas on the Cloudflare edition, end to end. The
// api Worker reserves each start in the organization's OrgDO before the
// start's D1 batch; with RUNBOOK_LIMIT_EXECUTIONS_MONTHLY=2 the third start
// this month gets 429 limit.executions_monthly and nothing is written, a
// retried start counts once, and the OrgDO holds the month's counter.
// LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { startWorker } from './harness.mjs';

const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');
let worker;
let db;
before(async () => {
  worker = await startWorker({
    faults: false,
    vars: {
      BOOTSTRAP_SECRET,
      ORIGIN: 'https://runbook.test',
      RUNBOOK_LIMIT_EXECUTIONS_MONTHLY: '2',
    },
  });
  db = await worker.mf.getD1Database('DB');
});
after(async () => {
  await worker?.dispose();
});

async function json(token, method, path, body, { key, ifMatch } = {}) {
  const headers = { authorization: `Bearer ${token}` };
  if (key) headers['idempotency-key'] = key;
  if (ifMatch) headers['if-match'] = ifMatch;
  if (body !== undefined) headers['content-type'] = 'application/json';
  const response = await worker.fetch(path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch {}
  return { status: response.status, body: parsed, text, etag: response.headers.get('etag') };
}

test('the third start this month is refused with 429 limit.executions_monthly; a retry counts once', async () => {
  const boot = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Quota Org', user_name: 'Owner', project_slug: 'ops' }),
  });
  assert.equal(boot.status, 201, await boot.clone().text());
  const out = await boot.json();
  const pat = out.personal_token;
  const project = out.project_id;
  const source = [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata: {id: verify, title: Verify}',
    `spec: {steps: [{id: check, title: Check, assignee: "user:${out.user_id}"}]}`,
    '',
  ].join('\n');
  const created = await json(pat, 'POST', `/v1/projects/${project}/runbooks`, { source }, { key: 'c1' });
  assert.equal(created.status, 201, created.text);
  const rb = created.body.runbook_id;
  const draft = await json(pat, 'GET', `/v1/runbooks/${rb}/draft`);
  const published = await json(pat, 'POST', `/v1/runbooks/${rb}/versions`, {}, { key: 'p1', ifMatch: draft.etag });
  assert.equal(published.status, 201, published.text);

  const first = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key: 's1' });
  assert.equal(first.status, 201, first.text);
  const retried = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key: 's1' });
  assert.equal(retried.body.id, first.body.id, 'a retried start answers the same run');
  const second = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key: 's2' });
  assert.equal(second.status, 201, second.text);
  const before = (await db.prepare('SELECT count(*) AS n FROM execution_owner').first()).n;
  const third = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key: 's3' });
  assert.equal(third.status, 429, third.text);
  assert.equal(third.body.code ?? third.body.error, 'limit.executions_monthly', third.text);
  const after = (await db.prepare('SELECT count(*) AS n FROM execution_owner').first()).n;
  assert.equal(after, before, 'a refused start writes nothing');
  // The running runs continue.
  const read = await json(pat, 'GET', `/v1/executions/${first.body.id}`);
  assert.equal(read.status, 200, read.text);
});
