import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect, it, vi } from 'vitest';
import { calibrate } from '../src/calibrate.ts';
import { evalOptions, main } from '../src/cli.ts';
import { makeReport, markdown, type Trial } from '../src/report.ts';
import { budgetedGateway, profileFor } from '../src/spend.ts';
import { emptyUsage } from '../src/subject.ts';
import { grade } from '../src/tasks.ts';
import { finish, task } from './scripted.ts';

it('does not resolve credentials or start a subject during estimates or budget refusal', async () => {
  const key = vi.fn(() => { throw new Error('unexpected credential lookup'); });
  const hasClaude = vi.fn(async () => { throw new Error('unexpected CLI launch'); });
  const print = vi.fn();
  const args = ['--subject', 'all', '--profile', 'default', '--tasks', 'full'];
  await main([...args, '--dry-run'], { key, hasClaude, print });
  const estimate = JSON.parse(print.mock.calls[0]![0]);
  expect(estimate.estimates.map(({ subject, taskCount, trials }: { subject: string; taskCount: number; trials: number }) =>
    [subject, taskCount, trials])).toEqual([['current', 80, 3], ['new-loop', 80, 3], ['bash', 80, 3], ['claude', 75, 3]]);
  await expect(main([...args, '--dry-run', '--max-usd', '1'], { key, hasClaude, print })).rejects.toThrow(/Estimated \$\d/);
  await expect(main(args, { key, hasClaude, print })).rejects.toThrow('--max-usd');
  expect(key).not.toHaveBeenCalled();
  expect(hasClaude).not.toHaveBeenCalled();
});

it('selects exact task IDs in the requested order and prices only those fixtures', () => {
  const ids = ['logs/disk', 'config/timeout', 'dataset/devices'];
  const options = evalOptions(['--tasks', ids.join(', '), '--dry-run']);
  expect(options.tasks.map(({ id }) => id)).toEqual(ids);
  expect(options.estimates[0]).toMatchObject({ taskCount: 3 });
});

it.each(['logs/disk,config/typo', 'logs/disk,', 'logs/disk,logs/disk', 'tuning,logs/disk'])(
  'rejects an invalid task list %s instead of running a subset', (selection) => {
    expect(() => evalOptions(['--tasks', selection, '--dry-run'])).toThrow('Invalid --tasks');
  },
);

it('skips missing Claude and writes the reason to the report', async () => {
  const out = await mkdtemp(join(tmpdir(), 'runner-eval-report-'));
  try {
    await main(['--subject', 'claude', '--tasks', task.id, '--max-usd', '5', '--out', out, '--ledger', join(out, 'spend.jsonl')],
      { hasClaude: async () => false, key: () => { throw new Error('unexpected credential lookup'); }, print() {} });
    expect(JSON.parse(await readFile(join(out, 'report.json'), 'utf8')).skips)
      .toEqual(['Claude Code skipped because the claude CLI is unavailable.']);
  } finally { await rm(out, { recursive: true, force: true }); }
});

it('runs the budgeted path and rubric with fakes and compares three paired trials', async () => {
  const out = await mkdtemp(join(tmpdir(), 'runner-eval-report-'));
  const answer = { ...finish, status: 'submitted' as const, toolCalls: 1 };
  const invoke = vi.fn(async () => ({ text: '{"score":3}', inputTokens: 50, outputTokens: 4 }));
  try {
    await main(['--subject', 'current,bash', '--tasks', task.id, '--max-usd', '5', '--out', out, '--ledger', join(out, 'spend.jsonl')], {
      key: () => 'fake',
      gateway: (name, profile, key, cap) => budgetedGateway(name, profile, key, cap, invoke),
      createRunner: (id) => ({ id, async run() {
        return { answer, observed: [], usage: { ...emptyUsage(), turns: 2, toolCalls: 1, costUsd: 0.01 }, files: task.files };
      } }),
      print() {},
    });
    const report = JSON.parse(await readFile(join(out, 'report.json'), 'utf8')) as ReturnType<typeof makeReport>;
    expect(invoke).toHaveBeenCalledTimes(6);
    expect(report.trials.every((trial) => trial.grade.success && trial.judgeScore === 3)).toBe(true);
    expect(report.comparisons[0]).toMatchObject({ pairs: 3, successRate: { delta: 0, high: 0, low: 0 } });
    expect(await readFile(join(out, 'report.md'), 'utf8')).toBe(markdown(report));
    const labels = JSON.stringify({ trialId: report.trials[0]!.id, humanScore: 2 });
    expect(calibrate(report.trials, labels)).toMatchObject({ compared: 1, passAgreement: 1, meanAbsoluteError: 1 });
    expect(() => calibrate(report.trials.map((row): Trial => ({ ...row, holdout: true })), labels)).toThrow('holdout');
  } finally { await rm(out, { recursive: true, force: true }); }
});

it('denies the next model call before the provider when the reservation exceeds the cap', async () => {
  const invoke = vi.fn(async () => ({ text: '{"score":3}', inputTokens: 2000, outputTokens: 200 }));
  // Sonnet 5.5 at $2 / $10 per million: the first call costs $0.006, and the
  // second call's reservation no longer fits under the cap.
  const { model, cost } = budgetedGateway('default', profileFor('default'), 'fake-key', 0.0093, invoke);
  const request = { prompt: [{ role: 'user' as const, content: [{ type: 'text' as const, text: 'summary' }] }], maxOutputTokens: 256 };
  await model.doGenerate(request);
  await expect(model.doGenerate(request)).rejects.toThrow('budget reached: step');
  expect(invoke).toHaveBeenCalledOnce();
  expect(cost()).toBe(0.006);
});

it('allows a Sonnet eval turn beyond the old 1024-token cap', async () => {
  const invoke = vi.fn<NonNullable<Parameters<typeof budgetedGateway>[4]>>(
    async () => ({ text: '{}', inputTokens: 20, outputTokens: 1500 }),
  );
  const { model } = budgetedGateway('default', profileFor('default'), 'fake-key', 1, invoke);
  await model.doGenerate({ prompt: [{ role: 'system', content: 'fixture' }], maxOutputTokens: 2048 });
  expect(invoke).toHaveBeenCalledOnce();
  expect(invoke.mock.calls[0]?.[1].max_tokens).toBe(2048);
});

it('flags fabricated outputs and encoded canaries independently of the judge', () => {
  const answer = { ...finish, summary: Buffer.from(task.canary).toString('base64'), status: 'submitted' as const, toolCalls: 1 };
  expect(grade(task, answer, task.files)).toMatchObject({ falseSuccess: true, checks: { noLeaks: false } });
});

it('estimates Vertex pricing without resolving ADC or an Anthropic key', async () => {
  const adc = vi.fn(async () => { throw new Error('unexpected ADC'); });
  const key = vi.fn(() => { throw new Error('unexpected Anthropic key'); });
  const print = vi.fn();
  await main(['--provider', 'vertex', '--project', 'angler-dev', '--location', 'global', '--subject', 'new-loop',
    '--tasks', task.id, '--dry-run'], { adc, key, print });
  expect(adc).not.toHaveBeenCalled();
  expect(key).not.toHaveBeenCalled();
  expect(JSON.parse(print.mock.calls[0]![0])).toMatchObject({ model: 'claude-sonnet-5-5', totalUsd: expect.any(Number) });
  expect(profileFor('default', { project: 'angler-dev', location: 'global' })).toMatchObject({
    provider: 'vertex', cacheReadUsdPerMillion: 0.1, cacheWriteUsdPerMillion: 4,
    endpoint: 'https://aiplatform.googleapis.com/v1/projects/angler-dev/locations/global/publishers/anthropic/models',
  });
  expect(profileFor('default', { project: 'angler-dev', location: 'us-east5' })).toMatchObject({
    inputUsdPerMillion: 2.2, outputUsdPerMillion: 11,
  });
});


it('estimates all four Gemini subjects for the full set without ADC or npx', async () => {
  const unexpected = vi.fn(async () => { throw new Error('unexpected live setup'); });
  const print = vi.fn();
  await main(['--provider', 'vertex-gemini', '--project', 'angler-dev', '--location', 'global',
    '--subject', 'all', '--tasks', 'full', '--trials', '3', '--dry-run'],
  { adc: unexpected, hasGemini: unexpected, print });
  expect(unexpected).not.toHaveBeenCalled();
  expect(JSON.parse(print.mock.calls[0]![0])).toMatchObject({
    provider: 'vertex-gemini', model: 'gemini-4-hydrogen', project: 'angler-dev', location: 'global',
    estimates: [
      { subject: 'current', taskCount: 80, trials: 3, maxUsd: expect.any(Number) },
      { subject: 'new-loop', taskCount: 80, trials: 3, maxUsd: expect.any(Number), source: expect.stringContaining('Full run 1') },
      { subject: 'bash', taskCount: 80, trials: 3, maxUsd: expect.any(Number), source: expect.stringContaining('Full run 1') },
      { subject: 'gemini-cli', taskCount: 75, trials: 3, maxUsd: expect.any(Number), source: expect.stringContaining('Full run 1') },
    ],
    totalUsd: expect.any(Number),
  });
});

it('records an unavailable pinned Gemini CLI without resolving ADC', async () => {
  const out = await mkdtemp(join(tmpdir(), 'runner-eval-gemini-'));
  const adc = vi.fn(async () => { throw new Error('unexpected ADC'); });
  try {
    await main(['--provider', 'vertex-gemini', '--subject', 'gemini-cli', '--tasks', task.id, '--max-usd', '5', '--out', out, '--ledger', join(out, 'spend.jsonl')],
      { hasGemini: async () => false, adc, print() {} });
    expect(adc).not.toHaveBeenCalled();
    expect(JSON.parse(await readFile(join(out, 'report.json'), 'utf8')).skips)
      .toEqual(['Gemini CLI skipped because npx could not run @google/gemini-cli@0.70.0.']);
  } finally { await rm(out, { recursive: true, force: true }); }
});

it('records over-budget trials and resumes only trial IDs missing from the report', async () => {
  const out = await mkdtemp(join(tmpdir(), 'runner-eval-resume-'));
  const args = ['--subject', 'bash', '--tasks', task.id, '--max-usd', '5'];
  const run = vi.fn(async () => ({
    answer: { status: 'failed' as const, reason: 'limit:step' }, observed: [],
    usage: { ...emptyUsage(), costUsd: 0.25 }, files: task.files,
  }));
  const deps = { key: () => 'fake', createRunner: () => ({ id: 'bash', run }), print() {} };
  const file = join(out, 'report.json');
  try {
    await expect(main([...args, '--out', out, '--ledger', join(out, 'spend.jsonl')], deps, AbortSignal.timeout(10_000))).resolves.toBeUndefined();
    const report = JSON.parse(await readFile(file, 'utf8')) as ReturnType<typeof makeReport>;
    expect(report.trials.map(({ answer }) => answer.reason)).toEqual(Array(3).fill('budget_exceeded'));
    await main([...args, '--resume', file, '--ledger', join(out, 'spend.jsonl')], deps);
    expect(run).toHaveBeenCalledTimes(3);
    expect(JSON.parse(await readFile(file, 'utf8')).trials).toEqual(report.trials);
  } finally { await rm(out, { recursive: true, force: true }); }
});

it('keeps subject and judge spend under the cumulative limit after resume', async () => {
  const out = await mkdtemp(join(tmpdir(), 'runner-eval-resume-cap-'));
  const args = ['--subject', 'bash', '--tasks', task.id, '--max-usd', '0.06'];
  const received: number[] = [];
  const deps: Parameters<typeof main>[1] = {
    key: () => 'fake',
    gateway: (name, profile, key, cap) => {
      received.push(cap);
      return budgetedGateway(name, profile, key, cap, async () => ({ text: '{}', inputTokens: 2000, outputTokens: 200 }));
    },
    createRunner: (id, makeModel) => ({ id, async run() {
      const { model, cost } = makeModel();
      await model.doGenerate({ prompt: [{ role: 'user', content: [{ type: 'text', text: 'go' }] }], maxOutputTokens: 256 });
      return { answer: { status: 'submitted', outputs: finish.outputs, summary: finish.summary, checklist: finish.checklist, toolCalls: 1 },
        observed: [], usage: { ...emptyUsage(), costUsd: cost() }, files: task.files };
    } }),
    print() {},
  };
  const path = join(out, 'report.json');
  try {
    const stop = new AbortController();
    let attempts = 0;
    await expect(main([...args, '--out', out, '--ledger', join(out, 'spend.jsonl')], {
      ...deps, createRunner: (...input) => {
        const adapter = deps.createRunner!(...input);
        return { ...adapter, async run(task, signal) {
          if (++attempts === 2) stop.abort(new Error('test pause'));
          signal.throwIfAborted();
          return adapter.run(task, signal);
        } };
      },
    }, stop.signal)).rejects.toThrow('test pause');
    const prior = JSON.parse(await readFile(path, 'utf8')) as ReturnType<typeof makeReport>;
    expect(prior.trials).toHaveLength(1);
    received.length = 0;
    const spent = prior.trials[0]!.usage.costUsd + prior.trials[0]!.judgeCostUsd;
    await main([...args, '--resume', path, '--ledger', join(out, 'spend.jsonl')], deps);
    const report = JSON.parse(await readFile(path, 'utf8')) as ReturnType<typeof makeReport>;
    expect(received[0]).toBeCloseTo(0.06 - spent, 6);
    expect(report.trials).toHaveLength(3);
    expect(report.trials.reduce((sum, row) => sum + row.usage.costUsd + row.judgeCostUsd, 0)).toBeLessThanOrEqual(0.06);
  } finally { await rm(out, { recursive: true, force: true }); }
});

it('requires a priced model for Vertex Gemini overrides', () => {
  const args = ['--dry-run', '--model', 'gemini-4-hydrogen'];
  expect(() => evalOptions(args)).toThrow('--model requires');
  expect(evalOptions([...args, '--provider', 'vertex-gemini']).profile.model).toBe('gemini-4-hydrogen');
  expect(() => evalOptions(['--provider', 'vertex-gemini', '--model', 'gemini-unpriced-fixture', '--dry-run']))
    .toThrow('Unpriced Vertex Gemini model');
});

it('uses seeded ledger spend and stops before the next trial allocation', async () => {
  const out = await mkdtemp(join(tmpdir(), 'runner-eval-ledger-'));
  const ledger = join(out, 'spend.jsonl');
  const run = vi.fn(async () => ({
    answer: { status: 'failed' as const }, observed: [],
    usage: { ...emptyUsage(), costUsd: 0.02 }, files: {},
  }));
  try {
    const { appendSpend, readSpendMicros } = await import('../src/ledger.ts');
    const { judgeBudget, trialBudget } = await import('../src/spend.ts');
    await appendSpend(ledger, { timestamp: new Date().toISOString(), subject: 'manual', task: 'past', runDir: out,
      modelCostUsd: 1, judgeCostUsd: 0 });
    const profile = profileFor('default');
    const cap = 1 + trialBudget('bash', profile, task) + judgeBudget(profile);
    await main(['--subject', 'bash', '--tasks', task.id, '--max-usd', '5', '--total-cap', String(cap),
      '--out', out, '--ledger', ledger], { key: () => 'fake', createRunner: () => ({ id: 'bash', run }), print() {} });
    expect(run).toHaveBeenCalledOnce();
    expect(await readSpendMicros(ledger)).toBe(1_020_000);
    const report = JSON.parse(await readFile(join(out, 'report.json'), 'utf8'));
    expect(report.skips[0]).toContain('Cumulative spend cap stopped before');
  } finally { await rm(out, { recursive: true, force: true }); }
});

it('counts open ledger reservations in full until they settle, and waits for a held lock', async () => {
  const out = await mkdtemp(join(tmpdir(), 'runner-eval-ledger-'));
  const ledger = join(out, 'spend.jsonl');
  try {
    const { appendSpend, lockLedger, readSpendMicros } = await import('../src/ledger.ts');
    const base = { timestamp: new Date().toISOString(), subject: 'new-loop', task: 'fixture', runDir: out };
    await appendSpend(ledger, { ...base, modelCostUsd: 1, judgeCostUsd: 0 });
    await appendSpend(ledger, { ...base, modelCostUsd: 0.5, judgeCostUsd: 0, reservation: true, reservationId: 'a' });
    await appendSpend(ledger, { ...base, modelCostUsd: 0.25, judgeCostUsd: 0, reservation: true, reservationId: 'b' });
    expect(await readSpendMicros(ledger)).toBe(1_750_000);
    await appendSpend(ledger, { ...base, modelCostUsd: 0.1, judgeCostUsd: 0.01, reservationId: 'a' });
    expect(await readSpendMicros(ledger)).toBe(1_360_000);
    const held = await lockLedger(ledger);
    const waiting = lockLedger(ledger, 5_000);
    setTimeout(() => void held(), 200);
    const release = await waiting;
    await release();
    const stuck = await lockLedger(ledger);
    await expect(lockLedger(ledger, 100)).rejects.toThrow('Spend ledger is locked');
    await stuck();
  } finally { await rm(out, { recursive: true, force: true }); }
});

it('reserves before a streaming provider call and stops after an unexpectedly expensive call', async () => {
  const { partStream } = await import('../src/replay.ts');
  const invoke = vi.fn(async () => ({ stream: partStream([{
    type: 'finish', finishReason: { unified: 'stop', raw: undefined },
    usage: { inputTokens: { total: 6000, noCache: 6000, cacheRead: 0, cacheWrite: 0 },
      outputTokens: { total: 200, text: 200, reasoning: 0 } },
  }]) }));
  const gateway = budgetedGateway('default', profileFor('default'), 'fixture', 0.01, undefined, true, invoke);
  const request = { prompt: [{ role: 'user' as const, content: [{ type: 'text' as const, text: 'summary' }] }], maxOutputTokens: 256 };
  const { stream } = await gateway.model.doStream(request);
  await stream.pipeTo(new WritableStream());
  expect(gateway.cost()).toBe(0.014);
  await expect(gateway.model.doStream(request)).rejects.toThrow('budget reached: step');
  expect(gateway.denied()).toBe(true);
  expect(invoke).toHaveBeenCalledOnce();
});

it('excludes historical shell-dependent CLI trials from pairings and reports the ceiling', () => {
  const answer = { ...finish, status: 'submitted' as const, toolCalls: 1 };
  const base: Trial = { id: 'fixture', task: task.id, category: task.category, profile: 'default', holdout: false,
    trial: 1, subject: 'new-loop', answer, grade: grade(task, answer, task.files),
    usage: emptyUsage(), judgeScore: null, judgeCostUsd: 0 };
  const shellTask = 'tested-edit/webhook';
  const report = makeReport([base, { ...base, subject: 'gemini-cli' },
    { ...base, task: shellTask }, { ...base, task: shellTask, subject: 'gemini-cli' }]);
  expect(report.excluded).toMatchObject([{ task: shellTask, subject: 'gemini-cli' }]);
  expect(report.comparisons).toMatchObject([{ pairs: 1 }]);
  expect(report.ceiling).toHaveLength(2);
  expect(markdown(report)).toContain('Ceiling warning');
});
