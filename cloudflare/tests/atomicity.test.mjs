// CF1 gate: one ExecutionDO commit is atomic. Faults injected partway
// through the commit (a Rust error, a real SQLite constraint failure, an
// error after the alarm was set, and a Rust panic) roll back every row and
// the alarm; the same object then commits cleanly.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { startWorker, executionId } from './harness.mjs';

let worker;
before(async () => {
  worker = await startWorker();
});
after(async () => {
  await worker?.dispose();
});

// Every ExecutionDO table (runbook-store-do's TABLES, from CF2's schema 0001 on).
const EMPTY = {
  meta: 0,
  event: 0,
  step_run: 0,
  timer: 0,
  timer_history: 0,
  outbox: 0,
  derived_outbox: 0,
  derived_assignment: 0, // CF13 (0006): a derived row's open assignments
  outbox_retry: 0,
  idempotency: 0,
  actor_replay: 0,
  claim: 0,
  claim_log: 0,
  // CF5: step log chunks moved to R2 (DO migration 0003).
  log_chunk: 0,
  start_record: 0,
  work_lease: 0,
  execution_credential: 0,
  credential_family: 0,
  alarm_log: 0,
  import_hold: 0, // gate fix D2 (0004)
};

async function state(id) {
  const { status, body } = await worker.object(id, '/state');
  assert.equal(status, 200);
  return body;
}

async function assertNothingVisible(id) {
  const body = await state(id);
  assert.equal(body.run, null, 'no execution head after rollback');
  assert.deepEqual(body.counts, EMPTY, 'no row in any ExecutionDO table');
  assert.equal(body.alarm_at_ms, null, 'the alarm set inside the transaction rolled back too');
}

async function assertCleanRetry(id) {
  const retry = await worker.object(id, '/start', { method: 'POST' });
  assert.equal(retry.status, 200);
  assert.equal(retry.body.committed, 3);
  const body = await state(id);
  assert.deepEqual(body.run.events, ['ExecutionStarted', 'StepReady', 'WaitStarted']);
  assert.equal(body.counts.event, 3);
  assert.equal(body.counts.meta, 1);
  assert.equal(body.counts.derived_outbox, 1);
  assert.equal(body.alarm_at_ms, body.run.timers[0].due_at_ms);
}

test('a clean start commits events, projection, timer, outbox, derived row and alarm together', async () => {
  const id = executionId();
  const started = await worker.object(id, '/start', { method: 'POST' });
  assert.deepEqual(started, { status: 200, body: { committed: 3 } });
  const body = await state(id);
  assert.equal(body.run.revision, 3);
  assert.equal(body.run.status, 'waiting');
  assert.deepEqual(body.run.steps, [{ path: 'pause', status: 'waiting' }]);
  assert.deepEqual(body.counts, { ...EMPTY, meta: 1, event: 3, step_run: 1, timer: 1, timer_history: 1, outbox: 1, derived_outbox: 1 });
  assert.equal(body.run.timers.length, 1);
  assert.equal(body.alarm_at_ms, body.run.timers[0].due_at_ms, 'the alarm is the earliest timer');
  // A second start fails the head check (expected sequence 0, stored 3).
  const again = await worker.object(id, '/start', { method: 'POST' });
  assert.deepEqual(again, { status: 409, body: { error: 'store.conflict' } });
  assert.equal((await state(id)).counts.event, 3);
});

for (const [fault, code] of [
  ['after_events', 'injected'],
  ['sql_constraint', 'do.write'],
  ['after_alarm', 'injected_after_alarm'],
]) {
  test(`fault ${fault}: the whole transaction rolls back and nothing is visible`, async () => {
    const id = executionId();
    const failed = await worker.object(id, `/start?fault=${fault}`, { method: 'POST' });
    assert.deepEqual(failed, { status: 500, body: { error: code } });
    await assertNothingVisible(id);
    await assertCleanRetry(id);
  });
}

test('fault panic: a Rust panic mid-commit rolls back; the instance is rebuilt for the next request', async () => {
  const id = executionId();
  // A wasm trap inside transactionSync unwinds through the request future
  // with panic=abort, so this request gets no response (CF2, D55). The
  // storage outcome is what the gate checks.
  const panicked = await worker
    .objectRaw(id, '/start?fault=panic', { method: 'POST', signal: AbortSignal.timeout(3000) })
    .then((response) => response.status, (error) => error.name);
  assert.notEqual(panicked, 200);
  await assertNothingVisible(id);
  await assertCleanRetry(id);
});

test('fault injection is refused unless the local test var enables it', async () => {
  const strict = await startWorker({ faults: false });
  try {
    const id = executionId();
    const refused = await strict.object(id, '/start?fault=after_events', { method: 'POST' });
    // Without the var the object has no spike hooks at all (CF3).
    assert.deepEqual(refused, { status: 404, body: { error: 'route.not_found' } });
  } finally {
    await strict.dispose();
  }
});
