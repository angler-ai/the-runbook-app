// CF2: the ExecutionDO store's suite inside workerd. The test Worker
// (crates/runbook-worker-conformance) runs each case in its own
// SQLite-backed Durable Object over `SqlStorage`, one RPC per case, through
// runbook-store-do's Workers backend: the same cases `cargo test -p
// runbook-worker-conformance` runs over native SQLite. Two lists: the shared
// execution-scoped conformance cases and the store's own (`do::…`): drain
// paging and backoff, the alarm mapping, credentials, ClaimStore,
// start/admit, the session fence, tenancy and migrations. CF3 adds the
// D1-scoped cases (directory, credentials, device codes, auth links,
// webhooks and memory) over runbook-store-d1 on Miniflare's local D1.
//
// The list of cases to run comes from the conformance crate itself
// (`EXECUTION_SCOPED`, compiled into the Worker), so a case the Worker does
// not register fails here instead of being skipped.
//
// CF5: the full shared suite. Every cross-store case (catalog, search,
// queue, retention, uploads, archives, language, parity: `CROSS_CASES`)
// runs over the composed Cloudflare store with every run in its own real
// ExecutionDO-style object (one `ConformanceDO` per run, reached through
// the object protocol), the local D1 and both R2 buckets, one RPC per case.
// Only derived rows may lag (the D56 bound, settled after each commit as
// the host does); everything else must hold at once.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { CONFORMANCE, startWorker } from './harness.mjs';

let worker;
let cases;
before(async () => {
  worker = await startWorker({ worker: CONFORMANCE, faults: false });
  const listed = await worker.json('/cases');
  assert.equal(listed.status, 200);
  cases = listed.body;
});
after(async () => {
  await worker?.dispose();
});

test('the Worker registers exactly the execution-scoped cases', () => {
  assert.ok(cases.execution_scoped.length >= 11, 'the conformance crate lists the cases');
  assert.deepEqual([...cases.shared].sort(), [...cases.execution_scoped].sort());
  assert.ok(cases.own.length >= 13, 'the store lists its own cases');
  assert.deepEqual([...cases.blob].sort(), [...cases.blob_cases].sort(), 'every blob case runs on R2');
  // CF3: every D1-scoped case of the conformance crate runs on D1.
  assert.ok(cases.d1_cases.length >= 36, 'the conformance crate lists the D1 cases');
  assert.deepEqual([...cases.d1].sort(), [...cases.d1_cases].sort(), 'every D1 case runs on D1');
  // CF3 review fixes: admission interleavings, one start per replay key and
  // the applier, driven on this host's D1.
  assert.ok(cases.d1_own.length >= 4, 'the D1 store lists its own cases');
  // CF5: every cross-store case of the conformance crate runs here.
  assert.ok(cases.cross_cases.length >= 65, 'the conformance crate lists the cross-store cases');
  assert.deepEqual([...cases.cross].sort(), [...cases.cross_cases].sort(), 'every cross-store case runs');
  assert.ok(cases.cross_own.length >= 1, 'the composed store lists its own cases');
});

test('every case passes inside workerd', async (t) => {
  const run = Date.now().toString(36);
  const single = [...cases.execution_scoped, ...cases.blob_cases, ...cases.own, ...cases.d1_cases, ...cases.d1_own];
  const cross = [...cases.cross_cases, ...cases.cross_own];
  for (const name of [...single, ...cross]) {
    await t.test(name, async () => {
      const response = await worker.fetch(`/case/${name}?run=${run}`, {
        method: 'POST',
        // A cross-store case makes many object RPCs (each run is its own
        // object), so it gets longer than a single-object case.
        signal: AbortSignal.timeout(cross.includes(name) ? 60_000 : 10_000),
      });
      const body = await response.json();
      assert.deepEqual({ status: response.status, body }, { status: 200, body: { ok: true, case: name } });
    });
  }
});

test('an unregistered case is refused, not skipped', async () => {
  const response = await worker.json('/case/execution::not_a_case', { method: 'POST' });
  assert.deepEqual(response, {
    status: 404,
    body: { error: 'case.unregistered', case: 'execution::not_a_case' },
  });
});
