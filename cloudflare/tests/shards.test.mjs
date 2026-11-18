// CF11: the D1 shard seam with two local D1 bindings. `DB` is the control
// database and the default shard; RUNBOOK_D1_SHARDS maps a second
// organization to `DB_2`. Everything that organization does lands on DB_2;
// the control rows (directory, credentials, token routes) stay on DB; no
// row of one organization reaches the other's shard or answers the other's
// requests; and the Cron passes sweep both shards. LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { root, startWorker, sleep, unzip } from './harness.mjs';

const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');
const ORG_B = 'org_01CCCCCCCCCCCCCCCCCCCCCCCC';
const USER_B = 'usr_01CCCCCCCCCCCCCCCCCCCCCCCC';
const PROJECT_B = 'prj_01CCCCCCCCCCCCCCCCCCCCCCCC';
// What stays on the control database (shards::D1Shards::control).
const CONTROL = new Set([
  'organization',
  'directory_revision',
  'user_identity',
  'org_user',
  'project',
  'team',
  'project_member',
  'team_member',
  'verified_email',
  'project_variables',
  'directory_request',
  'owner_recovery_audit',
  'auth_link',
  'invitation',
  'device_code',
  'credential',
  'execution_token_route',
  'instance_bootstrap',
]);

const source = (user) =>
  [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata: {id: verify, title: Verify}',
    `spec: {steps: [{id: check, title: Check the deploy, assignee: "user:${user}"}]}`,
    '',
  ].join('\n');

let worker;
let control;
let second;
const orgs = {};

async function until(check, ms = 15_000) {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() > deadline) throw new Error('timed out waiting');
    await sleep(100);
  }
}

async function json(token, method, path, body, { key, ifMatch } = {}) {
  const headers = {};
  if (token) headers.authorization = `Bearer ${token}`;
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

/** A credential for org B, cloned from org A's of the same kind and seeded
 * on the control database as bootstrap would. */
async function seedToken(kind, id, a) {
  const prefix = kind === 'personal' ? 'trb_pat_' : 'trb_svc_';
  const secret = `${prefix}${randomBytes(32).toString('base64url')}`;
  const hash = createHash('sha256').update(secret).digest();
  const template = await control
    .prepare("SELECT payload FROM credential WHERE json_extract(payload, '$.kind') = ?")
    .bind(kind)
    .first();
  const payload = JSON.parse(
    template.payload.replaceAll(a.org, ORG_B).replaceAll(a.user, USER_B).replaceAll(a.project, PROJECT_B),
  );
  payload.id = id;
  payload.hash = [...hash];
  delete payload.management;
  await control
    .prepare('INSERT INTO credential (id, org_id, secret_hash, payload) VALUES (?, ?, ?, ?)')
    .bind(id, ORG_B, hash.buffer.slice(hash.byteOffset, hash.byteOffset + 32), JSON.stringify(payload))
    .run();
  return secret;
}

/** Every table of `db` with an `org_id` column. */
async function orgTables(db) {
  const tables = await db
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'")
    .all();
  const out = [];
  for (const { name } of tables.results) {
    const columns = await db.prepare(`PRAGMA table_info(${name})`).all();
    if (columns.results.some((c) => c.name === 'org_id')) out.push(name);
  }
  return out.sort();
}

/** Rows per org in each org-keyed table of `db`: {table: {org: n}}. */
async function census(db) {
  const out = {};
  for (const table of await orgTables(db)) {
    const rows = await db.prepare(`SELECT org_id, count(*) AS n FROM ${table} GROUP BY org_id`).all();
    if (rows.results.length) out[table] = Object.fromEntries(rows.results.map((r) => [r.org_id, r.n]));
  }
  return out;
}

before(async () => {
  worker = await startWorker({
    faults: false,
    vars: {
      BOOTSTRAP_SECRET,
      WEBHOOK_SECRET_KEY: randomBytes(32).toString('base64'),
      WEBHOOKS_ALLOW_PRIVATE: 'on',
      ORIGIN: 'https://runbook.test',
      RUNBOOK_D1_SHARDS: JSON.stringify({ control: 'DB', default: 'DB', orgs: { [ORG_B]: 'DB_2' } }),
    },
    d1: { DB_2: { id: '00000000-0000-0000-0000-0000000000b2' } },
  });
  control = await worker.mf.getD1Database('DB');
  second = await worker.mf.getD1Database('DB_2');
});
after(async () => {
  await worker?.dispose();
});

test('org A bootstraps on the control database; org B is seeded there too', async () => {
  const response = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops' }),
  });
  assert.equal(response.status, 201, await response.clone().text());
  const out = await response.json();
  orgs.a = {
    org: out.org_id,
    user: out.user_id,
    project: out.project_id,
    pat: out.personal_token,
    service: out.service_token,
  };
  const a = orgs.a;
  await control.batch([
    control.prepare('INSERT INTO organization (org_id, title) VALUES (?, ?)').bind(ORG_B, 'Other'),
    control.prepare('INSERT INTO directory_revision (org_id, revision) VALUES (?, 1)').bind(ORG_B),
    control.prepare('INSERT INTO user_identity (user_id, name) VALUES (?, ?)').bind(USER_B, 'Other'),
    control
      .prepare("INSERT INTO org_user (org_id, user_id, name, active, org_role) VALUES (?, ?, 'Other', 1, 'owner')")
      .bind(ORG_B, USER_B),
    control
      .prepare("INSERT INTO project (project_id, org_id, slug, title) VALUES (?, ?, 'ops', 'Ops')")
      .bind(PROJECT_B, ORG_B),
    control
      .prepare("INSERT INTO project_member (org_id, project_id, user_id, role) VALUES (?, ?, ?, 'admin')")
      .bind(ORG_B, PROJECT_B, USER_B),
  ]);
  orgs.b = {
    org: ORG_B,
    user: USER_B,
    project: PROJECT_B,
    pat: await seedToken('personal', 'tok_01CCCCCCCCCCCCCCCCCCCCCCCP', a),
    service: await seedToken('service', 'tok_01CCCCCCCCCCCCCCCCCCCCCCCS', a),
  };
  // Control lookups (token hash → credential → directory) for both.
  for (const o of [orgs.a, orgs.b]) {
    const me = await json(o.pat, 'GET', '/v1/me');
    assert.equal(me.status, 200, me.text);
  }
});

test('each org publishes, starts a run, mints execution tokens and subscribes a webhook', async () => {
  for (const [name, o] of Object.entries(orgs)) {
    const hook = await json(o.pat, 'POST', `/v1/projects/${o.project}/webhooks`, {
      name: 'local',
      url: 'http://127.0.0.1:9/hook',
      events: ['*'],
    }, { key: `hook-${name}` });
    assert.equal(hook.status, 201, hook.text);
    o.webhook = hook.body.webhook.id;
    const created = await json(o.pat, 'POST', `/v1/projects/${o.project}/runbooks`, { source: source(o.user) }, { key: `create-${name}` });
    assert.equal(created.status, 201, created.text);
    o.runbook = created.body.runbook_id;
    const draft = await json(o.pat, 'GET', `/v1/runbooks/${o.runbook}/draft`);
    assert.equal(draft.status, 200, draft.text);
    const published = await json(o.pat, 'POST', `/v1/runbooks/${o.runbook}/versions`, {}, { key: `publish-${name}`, ifMatch: draft.etag });
    assert.equal(published.status, 201, published.text);
    const started = await json(o.pat, 'POST', `/v1/projects/${o.project}/executions`, { runbook: 'verify' }, { key: `start-${name}` });
    assert.equal(started.status, 201, started.text);
    o.exe = started.body.id;
    const read = await until(async () => {
      const r = await json(o.pat, 'GET', `/v1/executions/${o.exe}`);
      return r.status === 200 && r.body.steps.length > 0 && r;
    });
    o.step = read.body.steps[0].id;
    const minted = await json(o.service, 'POST', `/v1/executions/${o.exe}/tokens`, { session_id: `agent-${name}` }, { key: `mint-${name}` });
    assert.equal(minted.status, 201, minted.text);
    // The execution token is routed on the control database.
    const next = await json(minted.body.access_token, 'GET', `/v1/executions/${o.exe}/next`);
    assert.equal(next.status, 200, next.text);
    const tasks = await until(async () => {
      const r = await json(o.pat, 'GET', '/v1/me/tasks');
      return r.status === 200 && r.body.items.length > 0 && r;
    });
    assert.equal(tasks.body.items.length, 1, tasks.text);
    const search = await json(o.pat, 'GET', `/v1/projects/${o.project}/runbooks`);
    assert.equal(search.status, 200, search.text);
    assert.deepEqual(search.body.items.map((r) => r.id ?? r.runbook_id), [o.runbook], search.text);
  }
});

test("org B's rows are on DB_2, org A's on DB, the control rows on DB only", async () => {
  const onControl = await census(control);
  const onSecond = await census(second);
  const a = orgs.a.org;
  for (const [table, counts] of Object.entries(onSecond)) {
    assert.ok(!CONTROL.has(table), `control table ${table} has rows on DB_2: ${JSON.stringify(counts)}`);
    assert.deepEqual(Object.keys(counts), [ORG_B], `${table} on DB_2: ${JSON.stringify(counts)}`);
  }
  for (const [table, counts] of Object.entries(onControl)) {
    if (CONTROL.has(table)) continue;
    assert.deepEqual(Object.keys(counts), [a], `${table} on DB: ${JSON.stringify(counts)}`);
  }
  // Org B's data did land on DB_2, and its directory and tokens on DB.
  for (const table of ['runbook', 'runbook_version', 'execution_owner', 'webhook_subscription']) {
    assert.ok(onSecond[table]?.[ORG_B] > 0, `org B has ${table} rows on DB_2`);
    assert.ok(onControl[table]?.[a] > 0, `org A has ${table} rows on DB`);
  }
  for (const table of ['organization', 'project', 'credential', 'execution_token_route']) {
    assert.ok(onControl[table]?.[ORG_B] > 0, `org B's ${table} rows are on DB`);
  }
  // The run-keyed tables (no org_id column): each shard has its own org's
  // run only.
  for (const table of ['assignment', 'run_summary']) {
    for (const [db, own] of [[control, orgs.a.exe], [second, orgs.b.exe]]) {
      const rows = await db.prepare(`SELECT DISTINCT execution_id FROM ${table}`).all();
      assert.deepEqual(rows.results.map((r) => r.execution_id), [own], table);
    }
  }
  // The identity tables (no org_id column) are control rows only.
  for (const table of ['user_identity', 'verified_email']) {
    const rows = await second.prepare(`SELECT count(*) AS n FROM ${table}`).first();
    assert.equal(rows.n, 0, `${table} has rows on DB_2`);
  }
});

test("neither org's IDs answer the other's requests", async () => {
  const [a, b] = [orgs.a, orgs.b];
  for (const [me, other] of [[a, b], [b, a]]) {
    for (const path of [
      `/v1/executions/${other.exe}`,
      `/v1/step-runs/${other.step}`,
      `/v1/runbooks/${other.runbook}/draft`,
      `/v1/projects/${other.project}/runbooks`,
      `/v1/projects/${other.project}/webhooks/${other.webhook}/deliveries`,
    ]) {
      const response = await json(me.pat, 'GET', path);
      assert.ok([403, 404].includes(response.status), `${path}: ${response.status} ${response.text}`);
    }
    const own = await json(me.pat, 'GET', `/v1/executions/${me.exe}`);
    assert.equal(own.status, 200, own.text);
  }
});

test('the Cron passes sweep both shards', async () => {
  const handler = await worker.mf.getWorker();
  const minute = await handler.scheduled({ cron: '* * * * *', scheduledTime: new Date() });
  assert.equal(minute.outcome, 'ok', JSON.stringify(minute));
  // runbook.published (a project event) fans out to each org's own
  // subscription on its own shard.
  for (const o of Object.values(orgs)) {
    const deliveries = await until(async () => {
      const r = await json(o.pat, 'GET', `/v1/projects/${o.project}/webhooks/${o.webhook}/deliveries`);
      return r.status === 200 && r.body.items.length > 0 && r;
    });
    assert.ok(
      deliveries.body.items.some((d) => d.event_type === 'runbook.published'),
      deliveries.text,
    );
  }
  const outbox = await Promise.all(
    [control, second].map((db) => db.prepare('SELECT count(*) AS n FROM project_event_outbox').first()),
  );
  assert.deepEqual(outbox.map((r) => r.n), [0, 0], 'both shards drained');
  const daily = await handler.scheduled({ cron: '17 0 * * *', scheduledTime: new Date() });
  assert.equal(daily.outcome, 'ok', JSON.stringify(daily));
});

test('a CF8 import into an org on another shard: rows on its shard, directory on control', async () => {
  const { readFile } = await import('node:fs/promises');
  const { join } = await import('node:path');
  const SECRET = 'cf11-operator-secret-0123456789abcdef0123456789';
  const ORG = 'org_00000000000000000000000001';
  const PROJECT = 'prj_00000000000000000000000001';
  const fixture = await readFile(join(root, 'tests', 'fixtures', 'portable-fixture.zip'));
  const isolated = await startWorker({
    faults: false,
    vars: {
      RUNBOOK_OPERATOR_SECRET: SECRET,
      RUNBOOK_TEST_HOLD_WORK: 'on',
      RUNBOOK_D1_SHARDS: JSON.stringify({ orgs: { [ORG]: 'DB_2' } }),
    },
    d1: { DB_2: { id: '00000000-0000-0000-0000-0000000000b3' } },
  });
  try {
    const operator = (path, init = {}) =>
      isolated.fetch(path, { ...init, headers: { 'x-runbook-operator-secret': SECRET, ...(init.headers ?? {}) } });
    const imported = await operator('/internal/portable/import', {
      method: 'POST',
      headers: { 'content-type': 'application/zip' },
      body: fixture,
    });
    assert.equal(imported.status, 201, await imported.clone().text());
    const [one, two] = [await isolated.mf.getD1Database('DB'), await isolated.mf.getD1Database('DB_2')];
    const onOne = await census(one);
    const onTwo = await census(two);
    for (const [table, counts] of Object.entries(onTwo)) {
      assert.ok(!CONTROL.has(table), `control table ${table} has rows on DB_2`);
      assert.deepEqual(Object.keys(counts), [ORG], table);
    }
    for (const table of Object.keys(onOne)) {
      assert.ok(CONTROL.has(table), `shard table ${table} has rows on DB: ${JSON.stringify(onOne[table])}`);
    }
    assert.ok(onTwo.execution_owner?.[ORG] > 0 && onTwo.runbook?.[ORG] > 0, JSON.stringify(onTwo));
    assert.ok(onOne.organization?.[ORG] && onOne.project?.[ORG], JSON.stringify(onOne));
    // A second import is refused by the control database's guard.
    const again = await operator('/internal/portable/import', { method: 'POST', body: fixture });
    assert.equal(again.status, 409);
    // The export stitches the shard's rows and the control's directory
    // back into the same archive.
    const exported = await operator(`/internal/portable/export?org=${ORG}&project=${PROJECT}`);
    assert.equal(exported.status, 200, await exported.clone().text());
    const before = unzip(fixture);
    const after = unzip(await exported.arrayBuffer());
    assert.deepEqual([...after.keys()].sort(), [...before.keys()].sort());
    for (const [name, bytes] of before) {
      if (name !== 'manifest.json') assert.ok(after.get(name).equals(bytes), `${name} differs`);
    }
  } finally {
    await isolated.dispose();
  }
});
