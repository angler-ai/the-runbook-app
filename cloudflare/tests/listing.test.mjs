// UI-1c: the runbook list's search through the real D1 binding (workerd,
// Miniflare's local D1), not only the bundled SQLite of the native tests.
// The stored search terms are a BLOB matched with `instr`, and the tag
// pre-filter uses `json_each` (which SQLite before 3.45 cuts at a NUL), so
// the cases mirror the listing conformance fixture
// (runbook_store_conformance::listing): non-ASCII case folding, a
// description match past the 300 listed characters and one past the 2,000
// searched ones, a comma inside a tag, a slug-only match, a draft that does
// not parse, and a NUL inside a title and a tag (`q` past the NUL, `q`
// containing it, tag `a\0b` against `a\0c` and `a`). Each answer is what the
// reference (`ListingQuery::page`) gives, spelled out below; every query is
// walked to the end of its cursor with small pages, once with the listings
// the draft writes stored and once with every listing removed (the lazy
// backfill path, drafts read from R2). Also: the slug route
// (`GET /v1/projects/{p}/runbooks/{slug}`) answers the list's row.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { startWorker } from './harness.mjs';

const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');

const doc = (metadata) =>
  `apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: ${metadata}\nspec: {steps: [{id: s, title: S}]}\n`;
// slug -> draft source (the conformance fixture's).
const FIXTURE = {
  alpha: doc(
    `{id: alpha, title: Éclair Deploy, description: '${'x'.repeat(320)} needle', tags: [' prod ', prod, 'a,b', '']}`,
  ),
  beta: doc('{id: beta, tags: [ops]}'),
  gamma: '[',
  delta: doc('{id: delta, title: Delta, description: Rolls ÄPI back, tags: [ops, prod]}'),
  'epsilon-check': doc('{id: epsilon-check, title: Health}'),
  zeta: doc('{id: zeta, title: "Nul\\0 tail", tags: ["a\\0b", ops]}'),
  eta: doc(`{id: eta, title: Eta, description: '${'y'.repeat(2000)} farneedle'}`),
};
const ALL = Object.keys(FIXTURE);

// [query, the slugs the reference answers (any order; the list's order is
// runbook ID, newest first, checked separately)].
const CASES = [
  [{}, ALL],
  [{ q: 'éclair' }, ['alpha']],
  [{ q: ' ÉCLAIR ' }, ['alpha']],
  [{ q: 'needle' }, ['alpha']],
  [{ q: 'x needle' }, ['alpha']],
  [{ q: 'a,b' }, ['alpha']],
  [{ q: 'äpi' }, ['delta']],
  [{ q: 'epsilon' }, ['epsilon-check']],
  [{ q: 'gam' }, ['gamma']],
  [{ q: 'health' }, ['epsilon-check']],
  [{ q: 'ops' }, ['beta', 'delta', 'zeta']],
  [{ q: 'tail' }, ['zeta']],
  [{ q: 'nul\u0000 t' }, ['zeta']],
  [{ q: 'a\u0000b' }, ['zeta']],
  [{ q: 'a\u0000c' }, []],
  [{ q: 'yyyy' }, ['eta']],
  [{ q: 'farneedle' }, []],
  [{ q: 'nothing' }, []],
  [{ tag: 'a\u0000b' }, ['zeta']],
  [{ tag: 'a\u0000c' }, []],
  [{ tag: 'a' }, []],
  [{ tag: 'ops' }, ['beta', 'delta', 'zeta']],
  [{ tag: 'prod' }, ['alpha', 'delta']],
  [{ tag: ' ops , prod ' }, ['delta']],
  [{ tag: 'a\\,b,prod' }, ['alpha']],
  [{ tag: 'a,b' }, []],
  [{ tag: 'PROD' }, []],
  [{ q: 'e', tag: 'prod' }, ['alpha', 'delta']],
  [{ q: 'nul', tag: 'a\u0000b' }, ['zeta']],
];

let worker;
let db;
let pat;
let project;
const ids = {};
before(async () => {
  worker = await startWorker({ faults: false, vars: { BOOTSTRAP_SECRET, ORIGIN: 'https://runbook.test' } });
  db = await worker.mf.getD1Database('DB');
  const response = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops' }),
  });
  assert.equal(response.status, 201);
  ({ personal_token: pat, project_id: project } = await response.json());
  for (const [slug, source] of Object.entries(FIXTURE)) {
    const created = await get(`/v1/projects/${project}/runbooks`, {
      method: 'POST',
      key: `create-${slug}`,
      body: { source, slug },
    });
    assert.equal(created.status, 201, JSON.stringify(created.body));
    ids[slug] = created.body.runbook_id;
  }
});
after(async () => {
  await worker?.dispose();
});

async function get(path, { method = 'GET', key, body } = {}) {
  const headers = { authorization: `Bearer ${pat}` };
  if (key) headers['idempotency-key'] = key;
  if (body !== undefined) headers['content-type'] = 'application/json';
  const response = await worker.fetch(path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return { status: response.status, body: await response.json() };
}

/** Every row the list answers for `query`, following `next_cursor` to null. */
async function walk(query, limit) {
  const rows = [];
  let cursor;
  for (let pages = 0; pages < 50; pages++) {
    const params = new URLSearchParams({ ...query, limit: String(limit) });
    if (cursor) params.set('cursor', cursor);
    const page = await get(`/v1/projects/${project}/runbooks?${params}`);
    assert.equal(page.status, 200, `${JSON.stringify(query)}: ${JSON.stringify(page.body)}`);
    rows.push(...page.body.items);
    cursor = page.body.next_cursor;
    if (!cursor) return rows;
  }
  throw new Error(`${JSON.stringify(query)}: the walk does not end`);
}

/** The reference's order: runbook ID, newest (largest, byte order) first. */
const ordered = (slugs) =>
  [...slugs].sort((a, b) => (ids[a] < ids[b] ? 1 : ids[a] > ids[b] ? -1 : 0));

async function listedRows() {
  const row = await db
    .prepare('SELECT count(*) AS n FROM runbook WHERE listing IS NOT NULL AND listing_revision IS draft_revision')
    .first();
  return row.n;
}

async function checkAll(label) {
  for (const [query, expected] of CASES) {
    for (const limit of [2, 100]) {
      const rows = await walk(query, limit);
      assert.deepEqual(
        rows.map((r) => r.slug),
        ordered(expected),
        `${label}: ${JSON.stringify(query)} limit ${limit}`,
      );
    }
  }
}

test('q and tag through the real D1 answer as the reference, listings stored', async () => {
  // Whether this D1's json_each cuts a string at a NUL (SQLite before 3.45
  // does; D1 forbids sqlite_version()). The answers above hold either way.
  const { cut } = await db
    .prepare(`SELECT count(*) AS cut FROM json_each('["a\\u0000b"]') WHERE value = 'a'`)
    .first();
  console.log(`local D1 json_each cuts at a NUL: ${cut === 1}`);
  assert.equal(await listedRows(), ALL.length, 'every draft write stores its listing');
  await checkAll('stored');
  const [all] = [await walk({}, 100)];
  const row = (slug) => all.find((r) => r.slug === slug);
  assert.equal(row('alpha').title, 'Éclair Deploy');
  assert.deepEqual(row('alpha').tags, ['prod', 'a,b']);
  assert.equal(row('alpha').description_truncated, true);
  assert.equal([...row('alpha').description].length, 300);
  assert.equal(row('zeta').title, 'Nul\u0000 tail');
  assert.deepEqual(row('zeta').tags, ['a\u0000b', 'ops']);
  assert.equal(row('gamma').title, 'gamma', 'an unparsable draft lists its slug');
  assert.equal(row('beta').title, 'Beta', 'LANG7: derived from the ID');
});

test('q and tag through the real D1 answer as the reference, listings derived lazily', async () => {
  for (const [query, expected] of CASES) {
    await db.exec(
      'UPDATE runbook SET listing = NULL, listing_search = NULL, listing_digest = NULL, listing_revision = NULL, listing_format = NULL',
    );
    const rows = await walk(query, 2);
    assert.deepEqual(rows.map((r) => r.slug), ordered(expected), `unlisted: ${JSON.stringify(query)}`);
  }
  // A walk over every row stores every listing again.
  assert.equal(await listedRows(), ALL.length);
  await checkAll('re-stored');
});

test('a runbook by slug is the list row; a slug no runbook has is 404', async () => {
  const all = await walk({}, 100);
  for (const slug of ALL) {
    const one = await get(`/v1/projects/${project}/runbooks/${slug}`);
    assert.equal(one.status, 200, `${slug}: ${JSON.stringify(one.body)}`);
    assert.deepEqual(one.body, all.find((r) => r.slug === slug), slug);
    const byProjectSlug = await get(`/v1/projects/ops/runbooks/${slug}`);
    assert.deepEqual(byProjectSlug.body, one.body, `${slug} by project slug`);
  }
  for (const missing of ['alph', 'ALPHA', 'nope', 'a%00b', ids.alpha]) {
    const read = await get(`/v1/projects/${project}/runbooks/${missing}`);
    assert.equal(read.status, 404, `${missing}: ${JSON.stringify(read.body)}`);
    assert.equal(read.body.code, 'resource.not_found');
  }
});
