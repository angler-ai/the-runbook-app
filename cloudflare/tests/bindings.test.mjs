// CF1/CF3: the D1, R2 and Durable Object bindings from wrangler.jsonc are
// real (local) bindings in the Rust Worker: the first request migrates D1,
// and the api Worker answers with the real router.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { startWorker } from './harness.mjs';

let worker;
before(async () => {
  worker = await startWorker();
});
after(async () => {
  await worker?.dispose();
});

test('the first request migrates D1, and R2 is reachable', async () => {
  const meta = await worker.fetch('/v1/meta');
  assert.equal(meta.status, 200);
  const db = await worker.mf.getD1Database('DB');
  const applied = await db.prepare('SELECT count(*) AS n FROM schema_migration').first();
  assert.ok(applied.n >= 33, `migrations applied: ${applied.n}`);
  const bucket = await worker.mf.getR2Bucket('BLOBS');
  await bucket.put('cf3/probe.txt', 'probe');
  assert.equal(await (await bucket.get('cf3/probe.txt')).text(), 'probe');
});

test('health answers; unknown routes and malformed IDs are refused by the router', async () => {
  assert.equal((await worker.fetch('/health')).status, 200);
  const unknown = await worker.fetch('/nope');
  assert.ok([401, 404].includes(unknown.status), String(unknown.status));
  const malformed = await worker.fetch('/v1/executions/not-an-id');
  assert.ok([401, 404].includes(malformed.status), String(malformed.status));
  const body = await malformed.json();
  assert.ok(body.code, 'a problem document');
  assert.equal((await worker.fetch('/internal/bootstrap', { method: 'POST' })).status, 404, 'no bootstrap secret: no route');
  const weak = await startWorker({ faults: false, vars: { BOOTSTRAP_SECRET: 'short' } });
  try {
    assert.equal((await weak.fetch('/internal/bootstrap', { method: 'POST' })).status, 404, 'a short secret: no route');
  } finally {
    await weak.dispose();
  }
});
