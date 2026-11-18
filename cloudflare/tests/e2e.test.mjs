// CF3: the Cloudflare edition end to end, through the api Worker's real
// router (runbook_api::Router over D1, R2 and the ExecutionDOs), under
// workerd with Miniflare's local D1, R2 and Queues. LOCAL ONLY.
//
// An operator bootstraps an org with a PAT; the owner creates a webhook to
// a local receiver, publishes a runbook, starts a run, opens its SSE stream,
// claims and reports its step. The run summary and inbox rows are read from
// D1 directly, the SSE frames arrive, the webhook is delivered signed with
// the AES-GCM-sealed secret, and tenancy holds: a project-scoped token
// cannot reach another project's run, step run or stream (§18).
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { createHash, createHmac, randomBytes } from 'node:crypto';
import { createServer } from 'node:http';
import { startWorker, sleep, frames, parseFrame } from './harness.mjs';

const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');
const source = (user) =>
  [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata: {id: verify, title: Verify}',
    `spec: {steps: [{id: check, title: Check the deploy, assignee: "user:${user}"}]}`,
    '',
  ].join('\n');

let worker;
let db;
let receiver;
const received = [];
before(async () => {
  worker = await startWorker({
    faults: false,
    vars: {
      BOOTSTRAP_SECRET,
      WEBHOOK_SECRET_KEY: randomBytes(32).toString('base64'),
      WEBHOOKS_ALLOW_PRIVATE: 'on',
      ORIGIN: 'https://runbook.test',
      // CF5: the daily archive pass runs 91 days ahead (the last test), so
      // today's finished runs are archived and then expire live (D54).
      RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS: String(91 * 86_400_000),
    },
  });
  db = await worker.mf.getD1Database('DB');
  receiver = createServer((req, res) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      received.push({ headers: req.headers, body });
      res.writeHead(200, { 'content-type': 'text/plain' });
      res.end('ok');
    });
  });
  await new Promise((done) => receiver.listen(0, '127.0.0.1', done));
});
after(async () => {
  await worker?.dispose();
  await new Promise((done) => receiver?.close(done));
});

async function until(check, ms = 15_000) {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() > deadline) throw new Error('timed out waiting');
    await sleep(100);
  }
}

function call(token, method, path, body, { key, ifMatch } = {}) {
  const headers = {};
  if (token) headers.authorization = `Bearer ${token}`;
  if (key) headers['idempotency-key'] = key;
  if (ifMatch) headers['if-match'] = ifMatch;
  if (body !== undefined) headers['content-type'] = 'application/json';
  return worker.fetch(path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
}
async function json(token, method, path, body, options) {
  const response = await call(token, method, path, body, options);
  const text = await response.text();
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch {}
  return { status: response.status, body: parsed, text, etag: response.headers.get('etag') };
}

let pat;
let service;
let project;
let org;
let user;
let exe;
let sr;
let webhookSecret;
let webhookId;

test('the operator bootstraps the first org; again is refused', async () => {
  const refused = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': 'wrong', 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops' }),
  });
  assert.equal(refused.status, 401);
  const response = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops' }),
  });
  assert.equal(response.status, 201);
  const out = await response.json();
  assert.match(out.personal_token, /^trb_pat_/);
  ({ personal_token: pat, service_token: service, project_id: project, org_id: org, user_id: user } = out);
  const again = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Other', user_name: 'Other', project_slug: 'ops' }),
  });
  assert.equal(again.status, 409);
  // A handful of failed attempts a minute, then 429 (review 7; gate fix
  // B4: only failures count, so the right secret is never locked out).
  const statuses = [];
  for (let i = 0; i < 6; i += 1) {
    const guess = await worker.fetch('/internal/bootstrap', {
      method: 'POST',
      headers: { 'x-bootstrap-secret': `guess-${i}`, 'content-type': 'application/json' },
      body: '{}',
    });
    statuses.push(guess.status);
  }
  assert.ok(statuses.includes(429), `statuses: ${statuses}`);
  // Gate fix B4: the right secret is still answered (409: initialized).
  const owner = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Other', user_name: 'Other', project_slug: 'ops' }),
  });
  assert.equal(owner.status, 409);
  const me = await json(pat, 'GET', '/v1/me');
  assert.equal(me.status, 200, me.text);
});

test('a webhook subscription is created with a sealed secret', async () => {
  const url = `http://127.0.0.1:${receiver.address().port}/hook`;
  const created = await json(pat, 'POST', `/v1/projects/${project}/webhooks`, {
    name: 'local',
    url,
    events: ['*'],
  }, { key: 'webhook-1' });
  assert.equal(created.status, 201, created.text);
  webhookSecret = created.body.secret;
  webhookId = created.body.webhook.id;
  assert.match(webhookSecret, /^whsec_/);
  const stored = await db.prepare('SELECT body FROM webhook_subscription').first();
  assert.ok(!stored.body.includes(webhookSecret), 'only the sealed secret is stored');
  assert.match(JSON.parse(stored.body).secret, /^v2\./);
});

test('gate fix B3: hosts fetch would read as addresses are refused', async () => {
  for (const url of ['https://2130706433/hook', 'https://0x7f.1/hook', 'https://127.1/hook', 'https://%31%32%37.0.0.1/hook']) {
    const created = await json(pat, 'POST', `/v1/projects/${project}/webhooks`, { name: 'sneaky', url, events: ['*'] }, { key: `b3-${url}` });
    assert.equal(created.status, 422, `${url}: ${created.text}`);
    assert.equal(created.body.code, 'webhooks.url_invalid', url);
  }
});

test('publish, start, claim and report through the real router; D1 has the summary and inbox', async () => {
  const createdRunbook = await json(pat, 'POST', `/v1/projects/${project}/runbooks`, { source: source(user) }, { key: 'create-1' });
  assert.equal(createdRunbook.status, 201, createdRunbook.text);
  const rb = createdRunbook.body.runbook_id;
  const draft = await json(pat, 'GET', `/v1/runbooks/${rb}/draft`);
  assert.equal(draft.status, 200, draft.text);
  const published = await json(pat, 'POST', `/v1/runbooks/${rb}/versions`, {}, { key: 'publish-1', ifMatch: draft.etag });
  assert.equal(published.status, 201, published.text);

  const started = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key: 'start-1' });
  assert.equal(started.status, 201, started.text);
  exe = started.body.id;
  const retried = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key: 'start-1' });
  assert.equal(retried.body.id, exe, retried.text);

  const read = await until(async () => {
    const r = await json(pat, 'GET', `/v1/executions/${exe}`);
    return r.status === 200 && r.body.steps.length > 0 && r;
  });
  sr = read.body.steps[0].id;
  const summary = await db.prepare('SELECT status, sequence FROM run_summary WHERE execution_id = ?').bind(exe).first();
  assert.ok(summary, 'the run summary reached D1');
  const inbox = await db.prepare('SELECT step_run_id, kind FROM assignment WHERE execution_id = ?').bind(exe).all();
  assert.deepEqual(inbox.results.map((r) => r.step_run_id), [sr], 'the inbox row reached D1');
  const index = await db.prepare('SELECT execution_id FROM step_run_index WHERE step_run_id = ?').bind(sr).first();
  assert.equal(index.execution_id, exe);

  // CF2-9: an execution token minted by the service token is routed to its
  // run in D1 before the object stores it; the bearer then reaches the run
  // through the api Worker by that route alone.
  const minted = await json(service, 'POST', `/v1/executions/${exe}/tokens`, { session_id: 'agent-1' }, { key: 'mint-1' });
  assert.equal(minted.status, 201, minted.text);
  const routes = await db.prepare('SELECT execution_id FROM execution_token_route').all();
  assert.equal(routes.results.length, 2, 'access and refresh hashes are routed');
  assert.ok(routes.results.every((r) => r.execution_id === exe));
  const next = await json(minted.body.access_token, 'GET', `/v1/executions/${exe}/next`);
  assert.equal(next.status, 200, next.text);
  const open = await json(pat, 'GET', '/v1/me/tasks');
  assert.equal(open.status, 200, open.text);
  assert.deepEqual(open.body.items.map((t) => t.step_run_id ?? t.assignment?.step_run_id ?? t.id), [sr], open.text);
  const claim = await json(pat, 'POST', `/v1/step-runs/${sr}/claim`, { session_id: 'web' }, { key: 'claim-1' });
  assert.equal(claim.status, 200, claim.text);
  const reported = await json(pat, 'POST', `/v1/step-runs/${sr}/report`, {
    step_run_id: sr,
    claim: claim.body,
    status: 'succeeded',
    verdict: null,
    outputs: {},
    summary: 'done',
    evidence: [],
    logs: [],
  }, { key: 'report-1' });
  assert.equal(reported.status, 200, reported.text);

  const done = await json(pat, 'GET', `/v1/executions/${exe}`);
  assert.equal(done.body.status, 'succeeded', done.text);
  const final = await db.prepare('SELECT status FROM run_summary WHERE execution_id = ?').bind(exe).first();
  assert.equal(final.status, 'succeeded');
  const left = await db.prepare('SELECT count(*) AS n FROM assignment WHERE execution_id = ?').bind(exe).first();
  assert.equal(left.n, 0, 'a finished run leaves the inbox');
  const search = await json(pat, 'GET', `/v1/projects/${project}/executions`);
  assert.equal(search.body.items[0].id, exe, search.text);
  const tasks = await json(pat, 'GET', '/v1/me/tasks');
  assert.equal(tasks.status, 200, tasks.text);
});

test('the SSE stream replays the run and ends after its last event', async () => {
  const response = await call(pat, 'GET', `/v1/executions/${exe}/events`);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get('content-type'), 'text/event-stream');
  const stream = frames(response);
  const seen = [];
  for (;;) {
    const frame = await stream.next(5000);
    if (frame === null) break;
    const parsed = parseFrame(frame);
    if (parsed.event) seen.push(parsed.event);
  }
  assert.ok(seen.length >= 3, `frames: ${seen}`);
  assert.equal(seen.at(-1), 'ExecutionCompleted', `frames: ${seen}`);
  const anonymous = await call(null, 'GET', `/v1/executions/${exe}/events`);
  assert.equal(anonymous.status, 401);
});

test('D32: a stream is re-authorized at every keep-alive; revoking its token ends it', async () => {
  const minted = await json(pat, 'POST', '/v1/tokens', {
    name: 'watcher',
    scopes: ['executions:read'],
  }, { key: 'token-watcher' });
  assert.equal(minted.status, 201, minted.text);
  const secret = minted.body.secret ?? minted.body.token?.secret ?? minted.body.value;
  const tokenId = minted.body.token?.id ?? minted.body.id;
  const running = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key: 'start-2' });
  assert.equal(running.status, 201, running.text);
  const response = await call(secret, 'GET', `/v1/executions/${running.body.id}/events`);
  assert.equal(response.status, 200);
  const stream = frames(response);
  const first = await stream.next(5000);
  assert.ok(first && parseFrame(first).event, 'replayed frames arrive');
  // Keep-alives keep coming while the token is valid.
  for (;;) {
    const frame = await stream.next(5000);
    if (parseFrame(frame).comment === 'keep-alive') break;
  }
  const revoked = await json(pat, 'DELETE', `/v1/tokens/${tokenId}`);
  assert.ok([200, 204].includes(revoked.status), revoked.text);
  // The next recheck finds the token revoked and ends the stream.
  for (;;) {
    const frame = await stream.next(5000);
    if (frame === null) break;
    assert.notEqual(parseFrame(frame).comment, 'keep-alive', 'no keep-alive after revocation');
  }
});

test('D32 on a busy stream: events never postpone the re-check; revoking ends it', async () => {
  const minted = await json(pat, 'POST', '/v1/tokens', { name: 'busy-watcher', scopes: ['executions:read'] }, { key: 'token-busy' });
  assert.equal(minted.status, 201, minted.text);
  const secret = minted.body.secret ?? minted.body.token?.secret ?? minted.body.value;
  const tokenId = minted.body.token?.id ?? minted.body.id;
  const running = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key: 'start-busy' });
  assert.equal(running.status, 201, running.text);
  const busy = running.body.id;
  const step = await until(async () => {
    const r = await json(pat, 'GET', `/v1/executions/${busy}`);
    return r.body?.steps?.[0]?.id;
  });
  const response = await call(secret, 'GET', `/v1/executions/${busy}/events`);
  assert.equal(response.status, 200);
  const stream = frames(response);
  // Claim and release every ~150 ms, well inside the 400 ms keep-alive,
  // so the stream is never idle.
  let churning = true;
  let n = 0;
  const churn = (async () => {
    while (churning) {
      n += 1;
      const claim = await json(pat, 'POST', `/v1/step-runs/${step}/claim`, { session_id: `busy-${n}` }, { key: `busy-claim-${n}` });
      if (claim.status === 200) {
        await json(pat, 'POST', `/v1/step-runs/${step}/release`, { claim: claim.body }, { key: `busy-release-${n}` });
      }
      await sleep(150);
    }
  })();
  try {
    const deadline = Date.now() + 1200;
    while (Date.now() < deadline) {
      const frame = await stream.next(5000);
      assert.ok(frame !== null, 'the stream is live while the token is valid');
    }
    const revoked = await json(pat, 'DELETE', `/v1/tokens/${tokenId}`);
    assert.ok([200, 204].includes(revoked.status), revoked.text);
    const revokedAt = Date.now();
    for (;;) {
      const frame = await stream.next(5000);
      if (frame === null) break;
      assert.ok(Date.now() - revokedAt < 2500, 'the stream outlived its revoked token on a busy run');
    }
  } finally {
    churning = false;
    await churn;
    await stream.cancel();
  }
});

test('the webhook is delivered, signed with the secret, and logged in its WebhookDO (CF11)', async () => {
  const request = await until(() => received.find((r) => JSON.parse(r.body).execution?.id === exe));
  const timestamp = request.headers['runbook-timestamp'];
  const expected = createHmac('sha256', webhookSecret).update(`${timestamp}.${request.body}`).digest('hex');
  assert.equal(request.headers['runbook-signature'], `v1=${expected}`);
  const delivered = await until(async () => {
    const page = await json(pat, 'GET', `/v1/projects/${project}/webhooks/${webhookId}/deliveries`);
    return page.body?.items?.some((d) => d.state === 'delivered') && page;
  });
  assert.equal(delivered.status, 200, delivered.text);
  // The log is the subscription's object's, not D1's (1009).
  const table = await db
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'webhook_delivery'")
    .first();
  assert.equal(table, null);
});

test('tenancy: a token scoped to another project reaches none of this run (§18)', async () => {
  const other = await json(pat, 'POST', `/v1/orgs/${org}/projects`, { slug: 'web', title: 'Web' }, { key: 'project-2' });
  assert.equal(other.status, 201, other.text);
  const scoped = await json(pat, 'POST', '/v1/tokens', {
    name: 'web-only',
    scopes: ['executions:read', 'executions:act'],
    projects: [other.body.id],
  }, { key: 'token-2' });
  assert.equal(scoped.status, 201, scoped.text);
  const token = scoped.body.secret ?? scoped.body.token?.secret ?? scoped.body.value;
  assert.ok(token, scoped.text);
  for (const [method, path] of [
    ['GET', `/v1/executions/${exe}`],
    ['GET', `/v1/step-runs/${sr}`],
    ['GET', `/v1/executions/${exe}/report`],
  ]) {
    const r = await json(token, method, path);
    assert.ok([403, 404].includes(r.status), `${method} ${path}: ${r.status} ${r.text}`);
  }
  const stream = await call(token, 'GET', `/v1/executions/${exe}/events`);
  assert.ok([403, 404].includes(stream.status), `events: ${stream.status}`);
  await stream.body?.cancel();
  // An unknown or forged ID never reaches an object, and a garbage bearer
  // is refused before any lookup.
  const forged = await json(pat, 'GET', '/v1/executions/exe_00000000000000000000000000');
  assert.equal(forged.status, 404, forged.text);
  const garbage = await json('trb_pat_not-a-token', 'GET', `/v1/executions/${exe}`);
  assert.equal(garbage.status, 401, garbage.text);
});

test('CF9: runbook.published reaches the subscription through the minute cron', async () => {
  await fireMinuteCron();
  const published = await until(() =>
    received.find((r) => JSON.parse(r.body).type === 'runbook.published'),
  );
  const body = JSON.parse(published.body);
  assert.equal(body.execution, null);
  assert.equal(body.data.runbook_slug, 'verify', published.body);
  const left = await db.prepare('SELECT count(*) AS n FROM project_event_outbox').first();
  assert.equal(left.n, 0, 'the project outbox is drained');
});

async function fireMinuteCron() {
  const handler = await worker.mf.getWorker();
  const result = await handler.scheduled({ cron: '* * * * *', scheduledTime: new Date() });
  assert.equal(result.outcome, 'ok');
}

test('review fix 1: concurrent starts under one key make one run and one answer', async () => {
  const before = (await db.prepare('SELECT count(*) AS n FROM execution_owner').first()).n;
  for (let round = 0; round < 4; round += 1) {
    const key = `race-${round}`;
    const [a, b] = await Promise.all([
      json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key }),
      json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key }),
    ]);
    assert.equal(a.status, 201, a.text);
    assert.equal(b.status, 201, b.text);
    assert.equal(a.body.id, b.body.id, 'both callers get the same run');
  }
  const after = (await db.prepare('SELECT count(*) AS n FROM execution_owner').first()).n;
  assert.equal(after - before, 4, 'one run per key');
  // No second object started a run of its own: every run summary in D1
  // belongs to an owner row.
  const orphans = await db
    .prepare('SELECT count(*) AS n FROM run_summary s WHERE NOT EXISTS (SELECT 1 FROM start_replay r WHERE r.execution_id = s.execution_id)')
    .first();
  assert.equal(orphans.n, 0);
});

test('review fixes 2 and 6: the minute cron abandons a start whose object never initialized and drops dead notices', async () => {
  const version = await db.prepare('SELECT version_id FROM runbook_version LIMIT 1').first();
  const lost = 'exe_01AAAAAAAAAAAAAAAAAAAAAAAA';
  const old = Date.now() - 10 * 60_000;
  await db.batch([
    db.prepare('INSERT INTO execution_owner (execution_id, org_id, project_id) VALUES (?, ?, ?)').bind(lost, org, project),
    db
      .prepare(
        'INSERT INTO start_replay (org_id, project_id, idempotency_key, principal, request_hash, execution_id, outcome, created_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?)',
      )
      .bind(org, project, 'lost-key', 'human:x', new Uint8Array(32).buffer, lost,
        JSON.stringify({ execution_id: lost, runbook_version_id: version.version_id, runbook_version: 1, admission: null }), old),
    // A notice for a run no object holds: a terminal 404.
    db.prepare('INSERT INTO admission_notice (execution_id, org_id, project_id, admitted_at_ms) VALUES (?, ?, ?, ?)')
      .bind('exe_01BBBBBBBBBBBBBBBBBBBBBBBB', org, project, old),
  ]);
  await fireMinuteCron();
  const tomb = await until(() => db.prepare('SELECT body FROM tombstone WHERE execution_id = ?').bind(lost).first());
  assert.equal(JSON.parse(tomb.body).reason, 'start.abandoned');
  const replay = await db.prepare('SELECT count(*) AS n FROM start_replay WHERE execution_id = ?').bind(lost).first();
  assert.equal(replay.n, 0, 'the key is free again');
  const notice = await db.prepare('SELECT count(*) AS n FROM admission_notice').first();
  assert.equal(notice.n, 0, 'a notice no object can apply is dropped');
  // Like a run retention deleted: no owner row, a tombstone; the router
  // answers as natively, without reaching an object.
  const read = await call(pat, 'GET', `/v1/executions/${lost}`);
  assert.ok([404, 410].includes(read.status), String(read.status));
  assert.equal(read.headers.get('x-runbook-object'), null);
  await read.body?.cancel();
});

test('review fix 8: another org\'s IDs never reach an object', async () => {
  // A second organization with its own PAT, seeded in D1 as bootstrap would.
  const org2 = 'org_01CCCCCCCCCCCCCCCCCCCCCCCC';
  const user2 = 'usr_01CCCCCCCCCCCCCCCCCCCCCCCC';
  const project2 = 'prj_01CCCCCCCCCCCCCCCCCCCCCCCC';
  const secret = `trb_pat_${randomBytes(32).toString('base64url')}`;
  const hash = createHash('sha256').update(secret).digest();
  const template = await db.prepare("SELECT payload FROM credential WHERE json_extract(payload, '$.kind') = 'personal'").first();
  const payload = JSON.parse(template.payload.replaceAll(org, org2).replaceAll(user, user2));
  payload.id = 'tok_01CCCCCCCCCCCCCCCCCCCCCCCC';
  payload.hash = [...hash];
  delete payload.management;
  await db.batch([
    db.prepare('INSERT INTO organization (org_id, title) VALUES (?, ?)').bind(org2, 'Other'),
    db.prepare('INSERT INTO directory_revision (org_id, revision) VALUES (?, 1)').bind(org2),
    db.prepare('INSERT INTO user_identity (user_id, name) VALUES (?, ?)').bind(user2, 'Other'),
    db.prepare("INSERT INTO org_user (org_id, user_id, name, active, org_role) VALUES (?, ?, 'Other', 1, 'owner')").bind(org2, user2),
    db.prepare("INSERT INTO project (project_id, org_id, slug, title) VALUES (?, ?, 'ops', 'Ops')").bind(project2, org2),
    db.prepare("INSERT INTO project_member (org_id, project_id, user_id, role) VALUES (?, ?, ?, 'admin')").bind(org2, project2, user2),
    db.prepare('INSERT INTO credential (id, org_id, secret_hash, payload) VALUES (?, ?, ?, ?)').bind(payload.id, org2, hash.buffer.slice(hash.byteOffset, hash.byteOffset + 32), JSON.stringify(payload)),
  ]);
  const me = await json(secret, 'GET', '/v1/me');
  assert.equal(me.status, 200, me.text);
  // Control: the owning org's request is answered by the run's object.
  const own = await call(pat, 'GET', `/v1/executions/${exe}`);
  assert.equal(own.status, 200);
  assert.equal(own.headers.get('x-runbook-object'), '1');
  await own.body?.cancel();
  for (const path of [`/v1/executions/${exe}`, `/v1/step-runs/${sr}`, `/v1/executions/${exe}/report`]) {
    const response = await call(secret, 'GET', path);
    assert.equal(response.status, 404, path);
    assert.equal(response.headers.get('x-runbook-object'), null, `${path} reached an object`);
    await response.body?.cancel();
  }
  // A forged run ID and a client-sent object header change nothing.
  const forged = await worker.fetch('/v1/executions/exe_00000000000000000000000000', {
    headers: { authorization: `Bearer ${pat}`, 'x-runbook-new-id': '00000000000000000000000000' },
  });
  assert.equal(forged.status, 404);
  assert.equal(forged.headers.get('x-runbook-object'), null);
  // The refresh route answers an unknown run and a bad token alike.
  const unknown = await json(null, 'POST', '/v1/executions/exe_00000000000000000000000000/tokens/refresh', { refresh_token: 'trb_exr_bogus' });
  const bad = await json(null, 'POST', `/v1/executions/${exe}/tokens/refresh`, { refresh_token: 'trb_exr_bogus' });
  assert.deepEqual([unknown.status, unknown.body?.code], [bad.status, bad.body?.code]);
});

test('review fix 4: the alarm advances a waiting run through core drain, tenant from the object', async () => {
  const waiting = [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata: {id: waits, title: Waits}',
    'spec:',
    '  steps:',
    '    - {id: pause, wait: {for: 1s}}',
    `    - {id: after, title: After the wait, assignee: "user:${user}"}`,
    '',
  ].join('\n');
  const created = await json(pat, 'POST', `/v1/projects/${project}/runbooks`, { source: waiting }, { key: 'create-waits' });
  assert.equal(created.status, 201, created.text);
  const rb = created.body.runbook_id;
  const draft = await json(pat, 'GET', `/v1/runbooks/${rb}/draft`);
  const published = await json(pat, 'POST', `/v1/runbooks/${rb}/versions`, {}, { key: 'publish-waits', ifMatch: draft.etag });
  assert.equal(published.status, 201, published.text);
  const started = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'waits' }, { key: 'start-waits' });
  assert.equal(started.status, 201, started.text);
  const id = started.body.id;
  // No request touches the run: only its alarm can fire the timer.
  const after = await until(async () => {
    const row = await db.prepare('SELECT step_run_id FROM assignment WHERE execution_id = ?').bind(id).first();
    return row;
  }, 15_000);
  assert.ok(after.step_run_id, 'the step after the wait reached the inbox in D1');
  const read = await json(pat, 'GET', `/v1/executions/${id}`);
  const steps = read.body.steps.map((s) => [s.path, s.status]);
  assert.ok(steps.some(([path, status]) => path === 'pause' && status === 'succeeded'), JSON.stringify(steps));
});

async function publish(id, text) {
  const created = await json(pat, 'POST', `/v1/projects/${project}/runbooks`, { source: text }, { key: `create-${id}` });
  assert.equal(created.status, 201, created.text);
  const rb = created.body.runbook_id;
  const draft = await json(pat, 'GET', `/v1/runbooks/${rb}/draft`);
  const published = await json(pat, 'POST', `/v1/runbooks/${rb}/versions`, {}, { key: `publish-${id}`, ifMatch: draft.etag });
  assert.equal(published.status, 201, published.text);
  return rb;
}

test('CF5: a rerun lands in the original run\'s object and starts the new run in its own, over the object protocol', async () => {
  const rerun = await json(pat, 'POST', `/v1/executions/${exe}/rerun`, {}, { key: 'rerun-1' });
  assert.equal(rerun.status, 201, rerun.text);
  const id = rerun.body.id;
  assert.notEqual(id, exe);
  const again = await json(pat, 'POST', `/v1/executions/${exe}/rerun`, {}, { key: 'rerun-1' });
  assert.equal(again.body.id, id, 'a retried rerun answers the same run');
  const read = await json(pat, 'GET', `/v1/executions/${id}`);
  assert.equal(read.status, 200, read.text);
  assert.equal(read.body.rerun_of, exe, read.text);
  const owner = await db.prepare('SELECT org_id FROM execution_owner WHERE execution_id = ?').bind(id).first();
  assert.equal(owner.org_id, org);
  const summary = await until(() => db.prepare('SELECT status FROM run_summary WHERE execution_id = ?').bind(id).first());
  assert.ok(summary, 'the new run\'s summary reached D1');
});

test('CF5: cancel_previous cancels the holder in its own object and admits the new run', async () => {
  const text = [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata: {id: latest, title: Latest}',
    'spec:',
    '  execution: {concurrency: {key: prod, limit: 1, on_conflict: cancel_previous}}',
    `  steps: [{id: work, title: Work, assignee: "user:${user}"}]`,
    '',
  ].join('\n');
  await publish('latest', text);
  const first = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'latest' }, { key: 'latest-1' });
  assert.equal(first.status, 201, first.text);
  const second = await json(pat, 'POST', `/v1/projects/${project}/executions`, { runbook: 'latest' }, { key: 'latest-2' });
  assert.equal(second.status, 201, second.text);
  // The holder was cancelled by its own object, recorded as replaced.
  const holder = await until(async () => {
    const r = await json(pat, 'GET', `/v1/executions/${first.body.id}`);
    return r.status === 200 && r.body.status === 'cancelled' && r;
  });
  assert.equal(holder.body.superseded_by?.execution_id, second.body.id, holder.text);
  // The holder had no cleanup left: its slot freed and the new run holds it.
  const admission = await db.prepare('SELECT state FROM admission WHERE execution_id = ?').bind(second.body.id).first();
  assert.equal(admission.state, 'admitted');
  const released = await db.prepare('SELECT state FROM admission WHERE execution_id = ?').bind(first.body.id).first();
  assert.equal(released.state, 'released');
  const notices = await db.prepare('SELECT count(*) AS n FROM supersede_notice').first();
  assert.equal(notices.n, 0, 'the notice was delivered and dropped');
});

test('CF5: the daily cron archives finished runs, then retention deletes their live copies through the objects', async () => {
  const handler = await worker.mf.getWorker();
  const result = await handler.scheduled({ cron: '17 0 * * *', scheduledTime: new Date() });
  assert.equal(result.outcome, 'ok');
  // Today's finished runs are in a daily file in D1 and R2.
  const files = await db.prepare('SELECT runbook_id, day, stale FROM archive').all();
  assert.ok(files.results.length >= 1, 'a daily file was recorded');
  assert.ok(files.results.every((f) => f.stale === 0), JSON.stringify(files.results));
  const listed = await db.prepare('SELECT execution_id FROM archive_run WHERE execution_id = ?').bind(exe).first();
  assert.ok(listed, 'the finished run is listed in its file');
  const stored = await db.prepare('SELECT count(*) AS n FROM archive_version WHERE stored = 1 AND named = 1').first();
  assert.ok(stored.n >= 1, 'the named version is stored in R2');
  // 91 days on, its live copy is gone: tombstoned for retention, its object
  // purged and its D1 rows cascaded; the summary row stays with the file.
  const tombstone = await db.prepare("SELECT json_extract(body, '$.reason') AS reason FROM tombstone WHERE execution_id = ?").bind(exe).first();
  assert.equal(tombstone?.reason, 'retention.live_expired');
  const owner = await db.prepare('SELECT count(*) AS n FROM execution_owner WHERE execution_id = ?').bind(exe).first();
  assert.equal(owner.n, 0);
  const purges = await db.prepare('SELECT count(*) AS n FROM object_purge').first();
  assert.equal(purges.n, 0, 'every purged object was emptied');
  // CF11: its execution-token routes (control database) went with the purge.
  const routes = await db.prepare('SELECT count(*) AS n FROM execution_token_route WHERE execution_id = ?').bind(exe).first();
  assert.equal(routes.n, 0, 'the purge forgot the run\'s token routes');
  const read = await json(pat, 'GET', `/v1/executions/${exe}`);
  assert.ok([404, 410].includes(read.status), read.text);
});
