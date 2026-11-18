// CF13: a live run never outgrows a Durable Object row. Through the api
// Worker's real router under workerd, whose ExecutionDO SQLite holds at
// most 2,000,000 bytes a row. Before CF13 the run's head kept every step
// run's outputs and summary, so report 31 of a 40-step run with 65,000-byte
// summaries answered `503 storage.unavailable` (which clients retry
// forever), and a derived row kept every open assignment. LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { startWorker, sleep } from './harness.mjs';

const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');
let worker;
let owner;
before(async () => {
  worker = await startWorker({ faults: false, vars: { BOOTSTRAP_SECRET, ORIGIN: 'https://runbook.test' } });
  const boot = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Rows Org', user_name: 'Owner', project_slug: 'ops' }),
  });
  assert.equal(boot.status, 201, await boot.clone().text());
  owner = await boot.json();
});
after(async () => {
  await worker?.dispose();
});

async function call(method, path, body, { key, ifMatch } = {}) {
  const headers = { authorization: `Bearer ${owner.personal_token}` };
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

async function until(check, ms = 30_000) {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() > deadline) throw new Error('timed out waiting');
    await sleep(50);
  }
}

async function publishAndStart(slug, source, inputs = {}) {
  const created = await call('POST', `/v1/projects/${owner.project_id}/runbooks`, { source }, { key: `create-${slug}` });
  assert.equal(created.status, 201, created.text.slice(0, 500));
  const rb = created.body.runbook_id;
  const draft = await call('GET', `/v1/runbooks/${rb}/draft`);
  const published = await call('POST', `/v1/runbooks/${rb}/versions`, {}, { key: `publish-${slug}`, ifMatch: draft.etag });
  assert.equal(published.status, 201, published.text.slice(0, 500));
  const started = await call('POST', `/v1/projects/${owner.project_id}/executions`, { runbook: slug, inputs }, { key: `start-${slug}` });
  assert.equal(started.status, 201, started.text.slice(0, 500));
  return started.body.id;
}

const view = async (exe) => {
  const read = await call('GET', `/v1/executions/${exe}`);
  assert.equal(read.status, 200, read.text.slice(0, 500));
  return read.body;
};
const readyTasks = (body) => body.steps.filter((s) => s.status === 'ready' && s.kind === 'task');

async function claimAndReport(sr, key, report) {
  const claim = await call('POST', `/v1/step-runs/${sr}/claim`, { session_id: 'web' }, { key: `claim-${key}` });
  assert.equal(claim.status, 200, claim.text.slice(0, 500));
  return call('POST', `/v1/step-runs/${sr}/report`, {
    step_run_id: sr,
    claim: claim.body,
    status: 'succeeded',
    verdict: null,
    outputs: {},
    summary: 'done',
    evidence: [],
    logs: [],
    ...report,
  }, { key: `report-${key}` });
}

/** Work `count` task steps of `exe` one by one (each waits until ready). */
async function workThrough(exe, count, summary) {
  for (let n = 1; n <= count; n += 1) {
    const sr = await until(async () => readyTasks(await view(exe))[0]?.id);
    const reported = await claimAndReport(sr, `${exe}-${n}`, { summary });
    assert.equal(reported.status, 200, `report ${n}: ${reported.text.slice(0, 500)}`);
  }
  return until(async () => {
    const body = await view(exe);
    return body.status === 'succeeded' && body;
  });
}

const header = (id, title) => ['apiVersion: therunbook.app/v1', 'kind: Runbook', `metadata: {id: ${id}, title: ${title}}`];

test('a 40-step run with 65,000-byte summaries reports every step and completes', async () => {
  const steps = Array.from({ length: 40 }, (_, i) => `{id: s${i + 1}, title: Step ${i + 1}, assignee: "user:${owner.user_id}"}`);
  const source = [...header('long', 'Long'), `spec: {steps: [${steps.join(', ')}]}`, ''].join('\n');
  const exe = await publishAndStart('long', source);
  const done = await workThrough(exe, 40, 's'.repeat(65_000));
  assert.equal(done.steps.length, 40);
  assert.ok(done.steps.every((s) => s.summary.length === 65_000));
  const report = await call('GET', `/v1/executions/${exe}/report`);
  assert.equal(report.status, 200, report.text.slice(0, 500));
});

const wide = (id, concurrency) =>
  [
    ...header(id, 'Wide'),
    'spec:',
    '  inputs:',
    '    items: {type: list, items: {type: integer}}',
    '    note: {type: string}',
    '  steps:',
    '    - id: each',
    `      foreach: {items: '\${{ inputs.items }}', as: item, max_items: 100, concurrency: ${concurrency}, ` +
      `steps: [{id: check, title: 'Check \${{ item }} \${{ inputs.note }}', assignee: "user:${owner.user_id}"}]}`,
    '',
  ].join('\n');
const items = Array.from({ length: 100 }, (_, i) => i);

test('a foreach of 100 iterations with 25,000-byte summaries completes', async () => {
  const exe = await publishAndStart('wide', wide('wide', 1), { items, note: 'now' });
  const done = await workThrough(exe, 100, 's'.repeat(25_000));
  assert.equal(done.steps.filter((s) => s.kind === 'task').length, 100);
});

test('100 open assignments with 25,000-byte titles start, list and can be worked', async () => {
  const exe = await publishAndStart('titled', wide('titled', 100), { items, note: 'n'.repeat(25_000) });
  const open = await until(async () => {
    const body = await view(exe);
    return readyTasks(body).length === 100 && readyTasks(body);
  });
  const tasks = await until(async () => {
    const page = await call('GET', '/v1/me/tasks');
    return page.status === 200 && page.body.items.length > 0 && page.body.items;
  });
  assert.ok(tasks.every((t) => t.title.length > 25_000));
  const reported = await claimAndReport(open[0].id, 'titled-1', {});
  assert.equal(reported.status, 200, reported.text.slice(0, 500));
  await until(async () => readyTasks(await view(exe)).length === 99);
});

const wideOutputs = (id, then = []) => {
  const outputs = Array.from({ length: 40 }, (_, i) => `o${String(i + 1).padStart(2, '0')}: '\${{ steps.work.outputs.blob }}'`);
  return [
    ...header(id, 'Wide outputs'),
    'spec:',
    '  steps:',
    '    - id: work',
    '      title: Work',
    `      assignee: "user:${owner.user_id}"`,
    '      outputs: {blob: {type: string}}',
    ...then,
    `  outputs: {${outputs.join(', ')}}`,
    '',
  ].join('\n');
};

// CF13 review round 1, item 1: 40 outputs that each copy a 60,000-byte step
// output passed one row, so the completing report (and a cancel) answered
// 422 and the run could never end. The engine (version 5) holds a run's
// outputs together to 900 KiB: the report answers 200 and the run fails
// (execution.outputs_too_large), its outputs past the budget null.
test('a run whose outputs pass the budget fails instead of refusing its report', async () => {
  const exe = await publishAndStart('wideout', wideOutputs('wideout'));
  const sr = await until(async () => readyTasks(await view(exe))[0]?.id);
  const reported = await claimAndReport(sr, 'wideout', { outputs: { blob: 'b'.repeat(60_000) } });
  assert.equal(reported.status, 200, reported.text.slice(0, 500));
  const done = await until(async () => {
    const body = await view(exe);
    return body.status === 'failed' && body;
  });
  assert.equal(done.status, 'failed');
});

test('a run past the outputs budget can be cancelled', async () => {
  const exe = await publishAndStart('cancelout', wideOutputs('cancelout', [`    - {id: next, title: Next, assignee: "user:${owner.user_id}"}`]));
  const sr = await until(async () => readyTasks(await view(exe))[0]?.id);
  const reported = await claimAndReport(sr, 'cancelout', { outputs: { blob: 'b'.repeat(60_000) } });
  assert.equal(reported.status, 200, reported.text.slice(0, 500));
  await until(async () => readyTasks(await view(exe)).some((s) => s.path === 'next'));
  const cancelled = await call('PATCH', `/v1/executions/${exe}`, { action: 'cancel' }, { key: 'cancel-cancelout' });
  assert.equal(cancelled.status, 200, cancelled.text.slice(0, 500));
  await until(async () => (await view(exe)).status === 'cancelled');
});

// CF13 review round 1, item 2: a run's event log is held to a budget (8 MiB
// by default; measured, a report on a run there peaks near 61 MB). Reports
// carrying about 900 KB of logs each fill it in nine; the one that would
// pass it is a non-retryable 422 execution.too_large that writes nothing,
// and the run can still be finished with a short report, or cancelled.
test('a large run near the default run budget refuses the report that would pass it', async () => {
  const steps = Array.from({ length: 12 }, (_, i) => `{id: s${i + 1}, title: Step ${i + 1}, assignee: "user:${owner.user_id}"}`);
  const source = [...header('budget', 'Budget'), `spec: {steps: [${steps.join(', ')}]}`, ''].join('\n');
  const exe = await publishAndStart('budget', source);
  const logs = Array.from({ length: 15 }, () => 'l'.repeat(60_000));
  let accepted = 0;
  let refused = null;
  let sr = null;
  for (let n = 1; n <= 12 && !refused; n += 1) {
    sr = await until(async () => readyTasks(await view(exe))[0]?.id);
    const reply = await claimAndReport(sr, `budget-${n}`, { logs });
    if (reply.status === 200) {
      accepted += 1;
    } else {
      refused = reply;
    }
  }
  assert.ok(refused, 'a report past the budget is refused');
  assert.equal(refused.status, 422, refused.text.slice(0, 500));
  assert.equal(refused.body.code, 'execution.too_large');
  // About 900 KB a report: the log was near 8 MiB when the next was refused.
  assert.ok(accepted >= 8 && accepted <= 9, `${accepted} reports accepted`);
  const before = await view(exe);
  const claimed = before.steps.find((s) => s.id === sr);
  assert.equal(claimed.status, 'claimed');
  const again = await call('POST', `/v1/step-runs/${sr}/report`, {
    step_run_id: sr, claim: claimed.claim, status: 'succeeded', verdict: null, outputs: {}, summary: 'done', evidence: [], logs,
  }, { key: `report-budget-again` });
  assert.equal(again.status, 422, again.text.slice(0, 500));
  assert.deepEqual((await view(exe)).steps, before.steps);
  const small = await call('POST', `/v1/step-runs/${sr}/report`, {
    step_run_id: sr, claim: claimed.claim, status: 'succeeded', verdict: null, outputs: {}, summary: 'done', evidence: [], logs: ['ok'],
  }, { key: `report-budget-small` });
  assert.equal(small.status, 200, small.text.slice(0, 500));
  const cancelled = await call('PATCH', `/v1/executions/${exe}`, { action: 'cancel' }, { key: 'cancel-budget' });
  assert.equal(cancelled.status, 200, cancelled.text.slice(0, 500));
  await until(async () => (await view(exe)).status === 'cancelled');
});

// CF13b review round 1, finding 1: a run's activation settled with no
// ceiling, so `set` steps alone (20 of them, each setting 16 variables to a
// 60,000-byte input, about 960 KB a step, every row under 2 MB) left a run
// `running` at about 19.3 MB of log, which no later request could load
// within an isolate. Activation is now held to the run ceiling (12 MiB at
// the default 8 MiB budget): the run is faulted and closes failed in its
// activation commit, its person's step never opens, and it still answers.
test('an activation that passes the run ceiling closes the run failed', async () => {
  const names = Array.from({ length: 16 }, (_, i) => `a${String(i + 1).padStart(2, '0')}`);
  const assigned = names.map((n) => `${n}: '\${{ inputs.blob }}'`).join(', ');
  const source = [
    ...header('sets', 'Sets'),
    'spec:',
    '  inputs: {blob: {type: string, required: true}}',
    `  vars: {${names.map((n) => `${n}: ''`).join(', ')}}`,
    '  steps:',
    ...Array.from({ length: 20 }, (_, i) => `    - {id: s${i + 1}, set: {vars: {${assigned}}}}`),
    `    - {id: hold, title: Hold, assignee: "user:${owner.user_id}"}`,
    '',
  ].join('\n');
  const exe = await publishAndStart('sets', source, { blob: 'b'.repeat(60_000) });
  const done = await until(async () => {
    const body = await view(exe);
    return ['running', 'failed', 'succeeded', 'cancelled'].includes(body.status) && body;
  });
  assert.equal(done.status, 'failed');
  const hold = done.steps.find((s) => s.path === 'hold');
  assert.ok(!hold || !['ready', 'claimed'].includes(hold.status), JSON.stringify(hold));
  assert.equal(readyTasks(done).length, 0);
  // Closed in its activation: a cancel is the usual no-op, and it stays failed.
  const cancelled = await call('PATCH', `/v1/executions/${exe}`, { action: 'cancel' }, { key: 'cancel-sets' });
  assert.equal(cancelled.status, 200, cancelled.text.slice(0, 500));
  assert.equal((await view(exe)).status, 'failed');
});
