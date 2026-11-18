// CF8: the operator's project import and export on the api Worker, under
// workerd. The archive is the shared portable fixture
// (tests/fixtures/portable-fixture.zip, kept current by
// crates/runbook-worker-conformance/tests/portable.rs): imported through
// POST /internal/portable/import and exported again through
// GET /internal/portable/export, it comes back byte-identical apart from the
// manifest's created_at_ms. The routes exist only with the operator secret.
// LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { root, sleep, startWorker, unzip, zip } from './harness.mjs';

const SECRET = 'cf8-operator-secret-0123456789abcdef0123456789';
const ORG = 'org_00000000000000000000000001';
const PROJECT = 'prj_00000000000000000000000001';
let worker;
let fixture;
before(async () => {
  fixture = await readFile(join(root, 'tests', 'fixtures', 'portable-fixture.zip'));
  // The imported runs' work is held (RUNBOOK_TEST_HOLD_WORK), so the export
  // sees the archive as imported: a live import resumes them at once, which
  // delivers the fixture's pending notification.
  worker = await startWorker({
    faults: false,
    vars: { RUNBOOK_OPERATOR_SECRET: SECRET, RUNBOOK_TEST_HOLD_WORK: 'on' },
  });
});
after(async () => {
  await worker?.dispose();
});


const operator = (path, init = {}) =>
  worker.fetch(path, { ...init, headers: { 'x-runbook-operator-secret': SECRET, ...(init.headers ?? {}) } });

test('the routes are absent without the operator secret, and refuse a wrong one', async () => {
  const plain = await startWorker({ faults: false });
  try {
    const response = await plain.fetch(`/internal/portable/export?org=${ORG}&project=${PROJECT}`, {
      headers: { 'x-runbook-operator-secret': SECRET },
    });
    assert.equal(response.status, 404);
    assert.equal((await response.json()).code, 'resource.not_found');
  } finally {
    await plain.dispose();
  }
  const wrong = await worker.fetch('/internal/portable/import', {
    method: 'POST',
    headers: { 'x-runbook-operator-secret': 'nope' },
    body: fixture,
  });
  assert.equal(wrong.status, 401);
});

test('the fixture archive imports, and exports byte-identical apart from created_at_ms', async () => {
  const imported = await operator('/internal/portable/import', {
    method: 'POST',
    headers: { 'content-type': 'application/zip' },
    body: fixture,
  });
  assert.equal(imported.status, 201, await imported.clone().text());
  const summary = await imported.json();
  assert.equal(summary.org_id, ORG);
  assert.equal(summary.project_id, PROJECT);
  assert.ok(summary.runs >= 4, JSON.stringify(summary));

  const exported = await operator(`/internal/portable/export?org=${ORG}&project=${PROJECT}`);
  assert.equal(exported.status, 200, await exported.clone().text());
  assert.equal(exported.headers.get('content-type'), 'application/zip');
  const before = unzip(fixture);
  const afterFiles = unzip(await exported.arrayBuffer());
  assert.deepEqual([...afterFiles.keys()].sort(), [...before.keys()].sort());
  for (const [name, bytes] of before) {
    if (name === 'manifest.json') continue;
    assert.ok(afterFiles.get(name).equals(bytes), `${name} differs`);
  }
  const manifest = (files) => {
    const value = JSON.parse(files.get('manifest.json').toString('utf8'));
    delete value.created_at_ms;
    return value;
  };
  assert.deepEqual(manifest(afterFiles), manifest(before));

  // By slug too.
  const bySlug = await operator(`/internal/portable/export?org=${ORG}&project=nope-not-a-project`);
  assert.equal(bySlug.status, 404);
  assert.equal((await bySlug.json()).code, 'export.project_not_found');
});

test('a second import is refused, and a damaged archive never reaches the store', async () => {
  const again = await operator('/internal/portable/import', { method: 'POST', body: fixture });
  assert.equal(again.status, 409);
  assert.equal((await again.json()).code, 'import.org_exists');
  const into = await operator('/internal/portable/import?into_existing_org=true', {
    method: 'POST',
    body: fixture,
  });
  assert.equal(into.status, 409);
  assert.equal((await into.json()).code, 'import.target_not_empty');
  const damaged = Buffer.from(fixture);
  damaged[damaged.length - 30] ^= 0xff;
  const bad = await operator('/internal/portable/import', { method: 'POST', body: damaged });
  assert.equal(bad.status, 422, await bad.clone().text());
  const garbage = await operator('/internal/portable/import', { method: 'POST', body: 'not a zip' });
  assert.equal(garbage.status, 422);
});

test('gate fix D2: a live import (no work hold) commits, then releases every run', async () => {
  const live = await startWorker({ faults: false, vars: { RUNBOOK_OPERATOR_SECRET: SECRET } });
  try {
    const imported = await live.fetch('/internal/portable/import', {
      method: 'POST',
      headers: { 'x-runbook-operator-secret': SECRET, 'content-type': 'application/zip' },
      body: fixture,
    });
    assert.equal(imported.status, 201, await imported.clone().text());
    const summary = await imported.json();
    const db = await live.mf.getD1Database('DB');
    const pending = await db.prepare('SELECT count(*) AS n FROM run_release').first();
    assert.equal(pending.n, 0, 'every run was released after the commit');
    const owners = await db.prepare('SELECT execution_id FROM execution_owner').all();
    assert.equal(owners.results.length, summary.runs);
    // Each object answers as a released run: releasing again finds none held.
    for (const { execution_id: id } of owners.results) {
      const response = await live.objectRaw(id, '/op', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ execution_id: id, scope: { org_id: ORG, project_id: PROJECT }, op: 'release' }),
      });
      assert.deepEqual(await response.json(), { ok: false }, id);
    }
  } finally {
    await live.dispose();
  }
});

test('gate fix B4: wrong operator secrets are limited, the right one never is', async () => {
  const statuses = [];
  for (let i = 0; i < 7; i += 1) {
    const response = await worker.fetch(`/internal/portable/export?org=${ORG}&project=${PROJECT}`, {
      headers: { 'x-runbook-operator-secret': `wrong-${i}` },
    });
    statuses.push(response.status);
  }
  // Five failures a minute per client (one came from an earlier test).
  assert.equal(statuses[0], 401, String(statuses));
  assert.equal(statuses.at(-1), 429, String(statuses));
  const right = await operator(`/internal/portable/export?org=${ORG}&project=${PROJECT}`);
  assert.equal(right.status, 200, await right.clone().text());
});

// CF12: the import is one request under the Workers Paid limits, so it is
// bounded twice, both refusals coming before anything is written:
// - by subrequests: its worst case (two a run, blob or runbook source, plus
//   the D1 queries) must fit 10,000 less the route's reserve;
// - by memory: the archive is read whole, so it may be at most
//   MAX_ARCHIVE_BYTES as sent and as inflated (operator.rs).
const MiB = 1_048_576;
const MAX_ARCHIVE_BYTES = 10 * MiB;
const PAID_BUDGET = 10_000 - 100;

async function untouched(worker) {
  const db = await worker.mf.getD1Database('DB');
  const owners = await db.prepare('SELECT count(*) AS n FROM execution_owner').first();
  assert.equal(owners.n, 0, 'no run was committed');
  const projects = await db.prepare('SELECT count(*) AS n FROM project').first();
  assert.equal(projects.n, 0, 'no project was committed');
  const blobs = await (await worker.mf.getR2Bucket('BLOBS')).list();
  assert.deepEqual(blobs.objects.map((o) => o.key), [], 'no blob was written');
}

const importAt = async (worker, body) =>
  worker.fetch('/internal/portable/import', {
    method: 'POST',
    headers: { 'x-runbook-operator-secret': SECRET, 'content-type': 'application/zip' },
    body,
  });

test('CF12: an import past its subrequest budget writes nothing, and one at it imports', async () => {
  // The fixture's worst case, as the import reports it.
  const first = await startWorker({ faults: false, vars: { RUNBOOK_OPERATOR_SECRET: SECRET } });
  let cost;
  try {
    const response = await importAt(first, fixture);
    assert.equal(response.status, 201, await response.clone().text());
    const summary = await response.json();
    assert.equal(summary.subrequest_budget, PAID_BUDGET);
    cost = summary.subrequests;
    assert.ok(cost > 2 * (summary.runs + summary.blobs), JSON.stringify(summary));
  } finally {
    await first.dispose();
  }
  const short = await startWorker({
    faults: false,
    vars: { RUNBOOK_OPERATOR_SECRET: SECRET, RUNBOOK_TEST_IMPORT_SUBREQUESTS: String(cost - 1) },
  });
  try {
    const refused = await importAt(short, fixture);
    assert.equal(refused.status, 413, await refused.clone().text());
    assert.equal((await refused.json()).code, 'import.too_large');
    await untouched(short);
  } finally {
    await short.dispose();
  }
  const exact = await startWorker({
    faults: false,
    vars: { RUNBOOK_OPERATOR_SECRET: SECRET, RUNBOOK_TEST_IMPORT_SUBREQUESTS: String(cost) },
  });
  try {
    const imported = await importAt(exact, fixture);
    assert.equal(imported.status, 201, await imported.clone().text());
    const summary = await imported.json();
    assert.equal(summary.subrequests, cost);
    assert.equal(summary.subrequest_budget, cost);
  } finally {
    await exact.dispose();
  }
});

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

/** Replace `path`'s bytes in `files` and its size and digest in the manifest. */
function rewrite(files, path, bytes) {
  files.set(path, bytes);
  const manifest = JSON.parse(files.get('manifest.json').toString('utf8'));
  const entry = manifest.files.find((f) => f.path === path);
  entry.bytes = bytes.length;
  entry.sha256 = sha256(bytes);
  if (path === 'blobs.ndjson') entry.records = bytes.toString('utf8').split('\n').filter(Boolean).length;
  files.set('manifest.json', Buffer.from(JSON.stringify(manifest, null, 2)));
}

/** The fixture with one more blob, `filler` random bytes, for its first run. */
function withBlob(filler) {
  const files = unzip(fixture);
  const bytes = randomBytes(filler);
  const digest = sha256(bytes);
  const index = files.get('blobs.ndjson').toString('utf8').split('\n').filter(Boolean);
  const first = JSON.parse(index[0]);
  // `evidence/…` sorts before the fixture's first key, `logs/0001` of the
  // same run, so the index stays in canonical order.
  assert.equal(first.key.Execution.name, 'logs/0001');
  const run = first.key.Execution.execution_id;
  const row = { key: { Execution: { execution_id: run, name: 'evidence/cf12.bin' } }, sha256: digest, size: filler };
  files.set(`blobs/${digest}`, bytes);
  const manifest = JSON.parse(files.get('manifest.json').toString('utf8'));
  const tables = manifest.files.filter((f) => !f.path.startsWith('blobs/'));
  const blobs = [...manifest.files.filter((f) => f.path.startsWith('blobs/')), { path: `blobs/${digest}`, bytes: filler, sha256: digest }];
  blobs.sort((a, b) => (a.path < b.path ? -1 : 1));
  manifest.files = [...tables, ...blobs];
  files.set('manifest.json', Buffer.from(JSON.stringify(manifest, null, 2)));
  rewrite(files, 'blobs.ndjson', Buffer.from([JSON.stringify(row), ...index].join('\n') + '\n'));
  return files;
}

/** The fixture with `filler` more characters of step-log lines in its run that has some. */
function withLogs(filler) {
  const files = unzip(fixture);
  const lines = files.get('executions.ndjson').toString('utf8').split('\n').filter(Boolean);
  const at = lines.findIndex((line) => JSON.parse(line).claim_logs.length > 0);
  const run = JSON.parse(lines[at]);
  assert.equal(JSON.stringify(run), lines[at], 'the record re-encodes as it was');
  // Lines of 1,000 base64 characters (nothing to escape), then the rest.
  const text = randomBytes(Math.ceil(filler * 0.75) + 3).toString('base64');
  for (let start = 0; start < filler; start += 1_000) {
    run.claim_logs[0].lines.push(text.slice(start, Math.min(start + 1_000, filler)));
  }
  lines[at] = JSON.stringify(run);
  rewrite(files, 'executions.ndjson', Buffer.from(lines.join('\n') + '\n'));
  return files;
}

/**
 * CF12 review round 1: the fixture with `n` more tombstones, small rows
 * that each become a statement and bound values of the D1 commit batch.
 */
function withTombstones(n) {
  const files = unzip(fixture);
  const lines = files.get('tombstones.ndjson').toString('utf8').split('\n').filter(Boolean);
  const template = JSON.parse(lines[0]);
  for (let i = 0; i < n; i += 1) {
    lines.push(JSON.stringify({ ...template, execution_id: `exe_7${String(i).padStart(25, '0')}` }));
  }
  rewrite(files, 'tombstones.ndjson', Buffer.from(lines.join('\n') + '\n'));
  const manifest = JSON.parse(files.get('manifest.json').toString('utf8'));
  manifest.files.find((f) => f.path === 'tombstones.ndjson').records = lines.length;
  files.set('manifest.json', Buffer.from(JSON.stringify(manifest, null, 2)));
  return files;
}

/**
 * What the D1 commit batch costs on the JavaScript side, from the import's
 * answer: each text or BLOB copied (counted twice, for UTF-16), a value's
 * wrapper (64 B) and a statement's object (1 KiB).
 */
const batchBytes = (summary) => 2 * summary.bound_bytes + 64 * summary.bound_values + 1024 * summary.statements;

/** A stored archive of exactly `size` bytes, its bulk from `kind` (`withBlob` or `withLogs`). */
function archiveOf(kind, size) {
  let filler = size - 64 * 1024;
  for (let i = 0; i < 8; i += 1) {
    const body = zip(kind(filler));
    if (body.length === size) return body;
    filler += size - body.length;
  }
  throw new Error(`no ${kind.name} archive of ${size} bytes`);
}

for (const kind of [withBlob, withLogs]) {
  test(`CF12: a ${kind.name} archive of exactly the cap imports within the isolate memory; one byte more is refused unread`, async () => {
    const worker = await startWorker({ faults: false, vars: { RUNBOOK_OPERATOR_SECRET: SECRET } });
    try {
      const over = archiveOf(kind, MAX_ARCHIVE_BYTES + 1);
      const refused = await importAt(worker, over);
      assert.equal(refused.status, 413, await refused.clone().text());
      assert.equal((await refused.json()).code, 'request.body_too_large');
      await untouched(worker);

      const exact = archiveOf(kind, MAX_ARCHIVE_BYTES);
      const imported = await importAt(worker, exact);
      assert.equal(imported.status, 201, await imported.clone().text());
      // The isolate holds the wasm memory's high-water mark and JavaScript
      // copies of the request and of each object or R2 request (counted as
      // two archives): together well inside its 128 MB.
      const wasm = Number(imported.headers.get('x-runbook-test-wasm-bytes'));
      assert.ok(wasm > 0, 'the test build reports its wasm memory');
      const peak = wasm + 2 * exact.length + batchBytes(await imported.json());
      console.log(`CF12 ${kind.name} import of ${exact.length} B: wasm ${wasm} B, peak about ${peak} B`);
      assert.ok(peak <= 96 * MiB, `${peak} B is too close to the 128 MB isolate limit`);
    } finally {
      await worker.dispose();
    }
  });
}

test('CF12: a compressed archive is held to the cap once inflated', async () => {
  const worker = await startWorker({ faults: false, vars: { RUNBOOK_OPERATOR_SECRET: SECRET } });
  try {
    const files = withBlob(16);
    // The same blob, MAX_ARCHIVE_BYTES of zeros once inflated.
    const zeros = Buffer.alloc(MAX_ARCHIVE_BYTES);
    const digest = sha256(zeros);
    const old = [...files.keys()].find((k) => k.startsWith('blobs/') && files.get(k).length === 16);
    files.delete(old);
    const bomb = zip(new Map([...files, [`blobs/${digest}`, zeros]]), { deflate: true });
    assert.ok(bomb.length < MiB, `${bomb.length} B`);
    const refused = await importAt(worker, bomb);
    assert.equal(refused.status, 413, await refused.clone().text());
    assert.equal((await refused.json()).code, 'import.too_large');
    await untouched(worker);
  } finally {
    await worker.dispose();
  }
});

// CF12 review round 1: a deflated archive of many small rows (tombstones,
// up to the commit's statement bound) inflates past its size as sent and
// turns into a D1 batch of as many statements; the batch's bound values
// are counted on the JavaScript side too.
test('CF12: a deflated archive of many small rows imports within the isolate memory', async () => {
  const worker = await startWorker({ faults: false, vars: { RUNBOOK_OPERATOR_SECRET: SECRET } });
  try {
    const body = zip(withTombstones(9_700), { deflate: true });
    const imported = await importAt(worker, body);
    assert.equal(imported.status, 201, await imported.clone().text());
    const summary = await imported.json();
    assert.ok(summary.statements > 9_700 && summary.statements <= summary.statement_budget, JSON.stringify(summary));
    const wasm = Number(imported.headers.get('x-runbook-test-wasm-bytes'));
    assert.ok(wasm > 0, 'the test build reports its wasm memory');
    const inflated = [...unzip(body).values()].reduce((n, b) => n + b.length, 0);
    const peak = wasm + body.length + inflated + batchBytes(summary);
    console.log(`CF12 deflated rows import of ${body.length} B (${inflated} B inflated, ${summary.statements} statements): wasm ${wasm} B, peak about ${peak} B`);
    assert.ok(peak <= 96 * MiB, `${peak} B is too close to the 128 MB isolate limit`);
    // One row past the statement bound is refused before anything is written.
    const fresh = await startWorker({ faults: false, vars: { RUNBOOK_OPERATOR_SECRET: SECRET } });
    try {
      const over = zip(withTombstones(9_700 + summary.statement_budget - summary.statements + 1), { deflate: true });
      const refused = await importAt(fresh, over);
      assert.equal(refused.status, 413, await refused.clone().text());
      assert.equal((await refused.json()).code, 'import.too_large');
      await untouched(fresh);
    } finally {
      await fresh.dispose();
    }
  } finally {
    await worker.dispose();
  }
});

// CF12 review round 1: one import at a time per isolate, so two archives
// at the memory cap never share its 128 MB.
test('CF12: a second import while one runs in the isolate is refused as busy', async () => {
  const worker = await startWorker({ faults: false, vars: { RUNBOOK_OPERATOR_SECRET: SECRET } });
  try {
    const body = archiveOf(withBlob, MAX_ARCHIVE_BYTES);
    // The first import's body arrives in two halves, the second held back
    // until the other import has been answered: the first is running
    // (reading its body) throughout.
    let release;
    const held = new Promise((done) => {
      release = done;
    });
    let half = 0;
    const stream = new ReadableStream({
      async pull(controller) {
        if (half === 0) {
          half = 1;
          controller.enqueue(new Uint8Array(body.subarray(0, body.length / 2)));
          return;
        }
        await held;
        controller.enqueue(new Uint8Array(body.subarray(body.length / 2)));
        controller.close();
      },
    });
    const first = worker.fetch('/internal/portable/import', {
      method: 'POST',
      headers: { 'x-runbook-operator-secret': SECRET, 'content-type': 'application/zip' },
      body: stream,
      duplex: 'half',
    });
    await sleep(500);
    const second = await importAt(worker, body);
    release();
    assert.equal(second.status, 409, await second.clone().text());
    assert.equal((await second.json()).code, 'import.busy');
    const done = await first;
    assert.equal(done.status, 201, await done.clone().text());
    // Once it is done, the next one is let in (and refused for what it is).
    const after = await importAt(worker, body);
    assert.equal(after.status, 409);
    assert.equal((await after.json()).code, 'import.org_exists');
  } finally {
    await worker.dispose();
  }
});
