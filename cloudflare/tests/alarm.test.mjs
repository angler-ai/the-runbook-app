// CF1 gate: the DO alarm fires under workerd and its handler advances the
// run. The spike runbook waits 1 s; the start commit sets the alarm to the
// wait's timer, and the alarm's commit succeeds `pause` and readies `next`.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { startWorker, executionId, sleep } from './harness.mjs';

let worker;
before(async () => {
  worker = await startWorker();
});
after(async () => {
  await worker?.dispose();
});

test('the alarm fires and its handler advances the run past the wait', async (t) => {
  const id = executionId();
  const startedAt = Date.now();
  assert.equal((await worker.object(id, '/start', { method: 'POST' })).status, 200);
  const started = (await worker.object(id, '/state')).body;
  const dueAt = started.alarm_at_ms;
  assert.ok(dueAt >= startedAt + 1000 - 5, 'alarm is the 1 s wait timer');
  assert.deepEqual(started.alarm_log, [], 'not fired yet');

  let body;
  for (let i = 0; i < 100; i += 1) {
    await sleep(100);
    body = (await worker.object(id, '/state')).body;
    if (body.alarm_log.length > 0) break;
  }
  assert.ok(body.alarm_log.length > 0, 'alarm fired within 10 s');
  const [fired] = body.alarm_log;
  assert.match(fired.outcome, /^advanced:\d+$/);
  assert.ok(fired.fired_at_ms >= dueAt, 'fired at or after the due time');
  t.diagnostic(`alarm fired ${fired.fired_at_ms - dueAt} ms after its due time`);

  assert.deepEqual(
    body.run.steps.map((s) => [s.path, s.status]),
    [
      ['pause', 'succeeded'],
      ['next', 'ready'],
    ],
  );
  assert.equal(body.run.status, 'running');
  assert.ok(body.run.revision > started.run.revision);
  assert.ok(body.run.events.length > started.run.events.length);
  assert.deepEqual(body.run.timers, [], 'the fired timer is gone');
  assert.equal(body.counts.derived_outbox, 2, 'the advance wrote its derived row in the same commit');
  // CF2: no timer is left, but the outbox has undelivered rows (CF3/CF4
  // consume them), so the alarm is their retry: 10 s after the oldest one,
  // the start commit, whose 1 s wait timer set the first alarm.
  assert.deepEqual(body.pending.derived, [started.run.revision, body.run.revision]);
  // The start's derived row is stamped at its commit, a few ms after the
  // start time the wait's timer counts from.
  assert.ok(
    body.alarm_at_ms >= dueAt + 9_000 && body.alarm_at_ms < dueAt + 9_500,
    `the derived row's outbox retry (${body.alarm_at_ms - dueAt} ms after the wait)`,
  );
  assert.equal(body.next_alarm_at_ms, body.alarm_at_ms);
});

test('the outbox drains by cursor and acking it clears the alarm (object test hooks only)', async () => {
  const id = executionId();
  assert.equal((await worker.object(id, '/start', { method: 'POST' })).status, 200);
  const head = (await worker.object(id, '/state')).body.run.revision;
  // The api Worker has no route to the drain: its router refuses the path.
  for (const path of ['outbox/derived', 'outbox/derived/ack?through=0']) {
    const outside = await worker.fetch(`/v1/executions/${id}/${path}`, { method: path.includes('ack') ? 'POST' : 'GET' });
    assert.ok([401, 404, 405].includes(outside.status), `${path}: ${outside.status}`);
  }
  assert.deepEqual(await worker.object(id, '/outbox/derived'), { status: 200, body: { pending: [head] } });
  const beyond = await worker.object(id, `/outbox/derived/ack?through=${head + 1}`, { method: 'POST' });
  assert.deepEqual(beyond, { status: 409, body: { error: 'outbox.ack_beyond_head' } });
  const acked = await worker.object(id, `/outbox/derived/ack?through=${head}`, { method: 'POST' });
  assert.deepEqual(acked, { status: 200, body: { acked: 1 } });
  const again = await worker.object(id, `/outbox/derived/ack?through=${head}`, { method: 'POST' });
  assert.deepEqual(again, { status: 200, body: { acked: 0 } }, 'acks are idempotent');
  // CF4: the object already published its notifications to the queue and
  // acked them by cursor; a cursor-less ack is still refused.
  assert.deepEqual((await worker.object(id, '/outbox/notification')).body.pending, []);
  const bare = await worker.object(id, `/outbox/notification/ack?through=${head}`, { method: 'POST' });
  assert.deepEqual(bare, { status: 400, body: { error: 'outbox.invalid_cursor' } }, 'a notification ack names its cursor');
  const body = (await worker.object(id, '/state')).body;
  assert.deepEqual(body.pending, { derived: [], notification: [] });
  assert.equal(body.alarm_at_ms, body.run.timers[0].due_at_ms, 'only the wait timer is left');
  assert.equal(body.counts.derived_outbox, 1, 'acked rows stay with the run');
});

test('without the local test var the object serves no drain route', async () => {
  const strict = await startWorker({ faults: false });
  try {
    const id = executionId();
    // Without the var the object has no spike hooks at all (CF3).
    assert.deepEqual(await strict.object(id, '/start', { method: 'POST' }), {
      status: 404,
      body: { error: 'route.not_found' },
    });
    assert.deepEqual(await strict.object(id, '/outbox/derived/ack?through=3', { method: 'POST' }), {
      status: 404,
      body: { error: 'route.not_found' },
    });
    assert.deepEqual(await strict.object(id, '/outbox/derived'), { status: 404, body: { error: 'route.not_found' } });
  } finally {
    await strict.dispose();
  }
});
