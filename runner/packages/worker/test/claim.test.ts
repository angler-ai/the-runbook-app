import { CoreError } from '@runbook/runner-shared/core-client';
import { afterEach, expect, it, vi } from 'vitest';
import { admissionLoop } from '../src/admission.ts';
import { runClaim } from '../src/claim.ts';

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it('checks admission at startup, on a wake, and every thirty seconds', async () => {
  vi.useFakeTimers();
  const admit = vi.fn(async () => ({ items: [{ step_run_id: 'step', slot_id: 'slot' }] }));
  const run = vi.fn(async () => false);
  const worker = admissionLoop(1, admit, run, () => undefined);
  await vi.advanceTimersByTimeAsync(0);
  expect(admit).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(29_999);
  expect(admit).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1);
  expect(admit).toHaveBeenCalledTimes(2);
  worker.wake();
  await vi.advanceTimersByTimeAsync(0);
  expect(admit).toHaveBeenCalledTimes(3);
  expect(admit).toHaveBeenCalledWith(1);
  expect(run).toHaveBeenCalledWith({ step_run_id: 'step', slot_id: 'slot' }, expect.any(AbortSignal));
  await worker.close();
});

it('coalesces wake requests during an admission outage and backs off with jitter', async () => {
  vi.useFakeTimers();
  vi.spyOn(Math, 'random').mockReturnValue(0.5);
  const admit = vi.fn()
    .mockRejectedValueOnce(new Error('core unavailable'))
    .mockRejectedValueOnce(new Error('core unavailable'))
    .mockResolvedValue({ items: [] });
  const worker = admissionLoop(1, admit, async () => false, () => undefined);
  try {
    await vi.advanceTimersByTimeAsync(0);
    for (let i = 0; i < 10; i++) worker.wake();
    await vi.advanceTimersByTimeAsync(1499);
    expect(admit).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(admit).toHaveBeenCalledTimes(2);
    worker.wake();
    await vi.advanceTimersByTimeAsync(2999);
    expect(admit).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(admit).toHaveBeenCalledTimes(3);
  } finally {
    await worker.close();
  }
});

function fixture(refused = false) {
  const calls: string[] = [];
  let sandboxRunning = false;
  const release = vi.fn(async () => { expect(sandboxRunning).toBe(false); calls.push('slot.release'); });
  const refresh = vi.fn(async () => { calls.push('slot.heartbeat'); });
  const heartbeat = vi.fn(async () => {
    calls.push('core.heartbeat');
    if (heartbeat.mock.calls.length === 2) throw new CoreError(409, 'claim.stale_generation');
  });
  const deps: Parameters<typeof runClaim>[1] = {
    claim: async () => {
      calls.push('core.claim');
      if (refused) throw new Error('already claimed');
      return { id: 'claim', generation: 1 };
    },
    heartbeat, refresh, release,
    capability: async () => { calls.push('vault.capability'); return { token: 'fixture-grant', expiresAt: Date.now() + 600_000 }; },
    renewSandbox: async () => { calls.push('sandbox.renew'); },
    destroySandbox: async () => { sandboxRunning = false; calls.push('sandbox.destroy'); },
    report: async () => { calls.push('core.report'); },
    abandon: async () => { calls.push('core.release'); },
    async execute(ctx) {
      sandboxRunning = true;
      ctx.registerSandbox('sandbox');
      ctx.sandboxStarted();
      await new Promise<void>((resolve) => ctx.signal.addEventListener('abort', () => resolve(), { once: true }));
      calls.push('loop.aborted');
      return { status: 'failed', evidence: [], logs: [], outputs: {} };
    },
    heartbeatMs: 1000,
  };
  const run = () => runClaim({ step_run_id: 'step', slot_id: 'slot' }, deps, new AbortController().signal);
  return { calls, run, release, refresh, deps, heartbeat };
}

it('retries a transient slot release error', async () => {
  vi.useFakeTimers();
  const f = fixture();
  f.deps.claim = async () => { throw new Error('claim unavailable'); };
  f.release.mockRejectedValueOnce(new Error('scheduler restarting'));
  const running = f.run();
  const assertion = expect(running).resolves.toBe(false);
  await vi.advanceTimersByTimeAsync(1000);
  await assertion;
  expect(f.release).toHaveBeenCalledTimes(2);
});

it('failed_claim_releases_slot', async () => {
  const f = fixture(true);
  await expect(f.run()).resolves.toBe(false);
  expect(f.calls).toEqual(['core.claim', 'slot.release']);
});

it('heartbeats_while_running_and_stops_on_lease_loss', async () => {
  vi.useFakeTimers();
  const f = fixture();
  const running = f.run();
  await vi.advanceTimersByTimeAsync(1000);
  expect(f.calls).toContain('sandbox.renew');
  await vi.advanceTimersByTimeAsync(1000);
  await expect(running).resolves.toBe(false);
  expect(f.calls).toEqual([
    'core.claim', 'vault.capability', 'slot.heartbeat', 'core.heartbeat', 'vault.capability',
    'slot.heartbeat', 'sandbox.renew', 'core.heartbeat', 'loop.aborted', 'sandbox.destroy', 'slot.release',
  ]);
  await vi.advanceTimersByTimeAsync(60_000);
  expect(f.refresh).toHaveBeenCalledTimes(2);
});

it('stops a held step and releases its claim when the gateway loses the listener', async () => {
  vi.useFakeTimers();
  const f = fixture();
  f.deps.renewSandbox = async () => {
    throw Object.assign(new Error('sandbox unavailable'), { code: 'sandbox.gone' });
  };
  const running = f.run();
  try {
    await vi.advanceTimersByTimeAsync(1000);
    expect(f.calls.slice(-4)).toEqual(['loop.aborted', 'sandbox.destroy', 'core.release', 'slot.release']);
    expect(f.calls).not.toContain('core.report');
    expect(f.heartbeat).toHaveBeenCalledOnce();
  } finally {
    await vi.advanceTimersByTimeAsync(1000);
    await running;
  }
});

it('cost_reported_on_step_report after sandbox removal', async () => {
  const calls: string[] = [];
  const { workerClients } = await import('../src/clients.ts');
  const { loadConfig } = await import('@runbook/runner-shared/config');
  const token = 't'.repeat(32);
  const config = loadConfig('worker', {
    RUNNER_ROLE: 'worker', RUNNER_ID: 'r', RUNNER_GROUP_ID: 'g', CORE_URL: 'https://core.fixture',
    CORE_CLAIM_TOKEN: token, RUNNER_DATABASE_URL: 'postgres://localhost/runbook',
    API_URL: 'https://api.fixture', VAULT_URL: 'https://vault.fixture', GATEWAY_URL: 'https://gateway.fixture',
    SANDBOX_CTL_URL: 'https://sandbox.fixture', RUNNER_TOKEN_API_WORKER: token, RUNNER_TOKEN_WORKER_API: token,
    RUNNER_TOKEN_WORKER_VAULT: token, RUNNER_TOKEN_WORKER_GATEWAY: token, RUNNER_TOKEN_WORKER_SANDBOXCTL: token,
  });
  const expected = { model_usd: 0.1, compute_usd: 0.02, input_tokens: 30, output_tokens: 10, cached_tokens: 4 };
  let report: unknown;
  const fetcher: typeof fetch = async (input, init) => {
    const req = new Request(input, init);
    const path = new URL(req.url).pathname;
    calls.push(path);
    if (path.endsWith('/claim')) {
      expect(req.headers.get('idempotency-key')).toBeTruthy();
      expect(await req.json()).toMatchObject({ actor: { kind: 'agent', agent_name: 'r' } });
      return Response.json({ id: 'held', generation: 2 });
    }
    if (path.endsWith('/capability')) return Response.json({ capability: 'grant', expires_at: new Date(Date.now() + 600_000).toISOString() });
    if (path.endsWith('/cost')) return Response.json(expected);
    if (path.endsWith('/report')) report = await req.json();
    return new Response(null, { status: 204 });
  };
  const clients = workerClients(config, fetcher);
  expect(await runClaim({ step_run_id: 'step', slot_id: 'slot' }, {
    ...clients,
    async execute(ctx) {
      ctx.registerSandbox('sandbox');
      ctx.sandboxStarted();
      return { status: 'succeeded', evidence: [], logs: [], outputs: {} };
    },
  }, new AbortController().signal)).toBe(true);
  expect(calls).toEqual([
    '/v1/step-runs/step/claim', '/internal/capability', '/internal/slots/slot/heartbeat',
    '/internal/sandboxes/sandbox', '/internal/slots/slot/cost', '/v1/step-runs/step/report', '/internal/end-claim', '/internal/slots/slot/release',
  ]);
  expect(report).toMatchObject({ cost: expected, claim: { id: 'held', generation: 2 } });
});

it('retries reporting the same completed outcome when cost and first report fail', async () => {
  vi.useFakeTimers();
  const execute = vi.fn(async () => ({
    status: 'succeeded' as const, evidence: [], logs: [], outputs: {},
  }));
  const report = vi.fn<Parameters<typeof runClaim>[1]['report']>()
    .mockRejectedValueOnce(new Error('temporary failure')).mockResolvedValue(undefined);
  const abandon = vi.fn(async () => undefined);
  const release = vi.fn(async () => undefined);
  const shutdown = new AbortController();
  const run = runClaim({ step_run_id: 'step', slot_id: 'slot' }, {
    claim: async () => ({ id: 'held', generation: 1 }),
    heartbeat: async () => undefined, capability: async () => ({ token: 'grant', expiresAt: Date.now() + 600_000 }),
    refresh: async () => undefined, renewSandbox: async () => undefined,
    destroySandbox: async () => undefined,
    cost: async () => { throw new Error('cost unavailable'); },
    execute, report, abandon, release,
  }, shutdown.signal);
  await vi.advanceTimersByTimeAsync(0);
  expect(report).toHaveBeenCalledOnce();
  shutdown.abort();
  await vi.advanceTimersByTimeAsync(2000);
  await expect(run).resolves.toBe(true);
  expect(execute).toHaveBeenCalledTimes(1);
  expect(report).toHaveBeenCalledTimes(2);
  expect(report.mock.calls[0]?.[2]).toEqual(await execute.mock.results[0]?.value);
  expect(report.mock.calls[0]?.[4]).toBeTruthy();
  expect(report.mock.calls[1]?.[4]).toBe(report.mock.calls[0]?.[4]);
  expect(abandon).not.toHaveBeenCalled();
  expect(release).toHaveBeenCalledTimes(1);
});

it('a transient heartbeat failure leaves the claim running until a retry', async () => {
  vi.useFakeTimers();
  const f = fixture();
  f.heartbeat.mockReset()
    .mockRejectedValueOnce(new Error('temporarily unavailable'))
    .mockResolvedValueOnce(undefined)
    .mockRejectedValueOnce(new CoreError(409, 'claim.stale_generation'));
  const running = f.run();
  await vi.advanceTimersByTimeAsync(1000);
  expect(f.calls).not.toContain('loop.aborted');
  await vi.advanceTimersByTimeAsync(1000);
  expect(f.refresh).toHaveBeenCalledTimes(2);
  await vi.advanceTimersByTimeAsync(1000);
  await expect(running).resolves.toBe(false);
  expect(f.release).toHaveBeenCalledOnce();
});

it('leaves slot release to the reaper when repeated sandbox deletion fails', async () => {
  vi.useFakeTimers();
  const f = fixture();
  f.deps.execute = async (ctx) => {
    ctx.registerSandbox('sandbox');
    ctx.sandboxStarted();
    return { status: 'succeeded', evidence: [], logs: [], outputs: {} };
  };
  const destroy = vi.fn(async () => { throw new Error('controller unavailable'); });
  f.deps.destroySandbox = destroy;
  const running = f.run();
  await vi.advanceTimersByTimeAsync(2000);
  await expect(running).resolves.toBe(true);
  expect(destroy).toHaveBeenCalledTimes(6);
  expect(f.release).not.toHaveBeenCalled();
  expect(f.calls).toContain('core.report');
  expect(f.calls).not.toContain('core.release');
});

it('stops retrying near the last valid lease deadline when the core stays unavailable', async () => {
  vi.useFakeTimers();
  const f = fixture();
  f.deps.capability = async () => ({ token: 'grant', expiresAt: Date.now() + 3500 });
  f.heartbeat.mockReset().mockRejectedValue(new Error('temporarily unavailable'));
  const running = f.run();
  await vi.advanceTimersByTimeAsync(2999);
  expect(f.calls).not.toContain('loop.aborted');
  await vi.advanceTimersByTimeAsync(1);
  await expect(running).resolves.toBe(false);
  expect(f.heartbeat).toHaveBeenCalledTimes(2);
  expect(f.release).toHaveBeenCalledOnce();
});

it('reaps a revoked decision lease mid-turn without posting a decision or an abstention', async () => {
  vi.useFakeTimers();
  const f = fixture();
  f.deps.finishDecision = vi.fn(async () => undefined);
  const running = runClaim({ kind: 'decision', step_run_id: 'step', slot_id: 'slot' }, f.deps, new AbortController().signal);
  await vi.advanceTimersByTimeAsync(2000);
  await expect(running).resolves.toBe(false);
  expect(f.calls.slice(-4)).toEqual(['core.heartbeat', 'loop.aborted', 'sandbox.destroy', 'slot.release']);
  expect(f.deps.finishDecision).not.toHaveBeenCalled();
  expect(f.calls).not.toContain('core.report');
});

it('treats approval.not_open as a finished race without retrying or sending a task report', async () => {
  const f = fixture();
  f.deps.execute = async (ctx) => {
    expect(ctx.kind).toBe('decision');
    return { status: 'succeeded', evidence: [], logs: [], outputs: {},
      decision: { option: 'hold', comment: 'Evidence [call 1]', evidence: ['[call 1]'] } };
  };
  f.deps.finishDecision = vi.fn(async () => { throw new CoreError(409, 'approval.not_open'); });
  await expect(runClaim({ kind: 'decision', step_run_id: 'step', slot_id: 'slot' }, f.deps, new AbortController().signal))
    .resolves.toBe(true);
  expect(f.deps.finishDecision).toHaveBeenCalledTimes(1);
  expect(f.calls).not.toContain('core.report');
  expect(f.release).toHaveBeenCalledTimes(1);
});

it('records a rejected decision as an abstention on the same lease', async () => {
  vi.useFakeTimers();
  vi.spyOn(Math, 'random').mockReturnValue(0.5);
  const f = fixture();
  f.deps.execute = vi.fn(async () => ({
    status: 'succeeded' as const, evidence: [], logs: [], outputs: {},
    decision: { option: 'hold', comment: 'Evidence', evidence: ['[call 1]'] },
  }));
  const finishDecision = vi.fn<NonNullable<typeof f.deps.finishDecision>>()
    .mockRejectedValueOnce(new CoreError(422, 'approval.invalid_evidence'))
    .mockResolvedValue(undefined);
  f.deps.finishDecision = finishDecision;
  const running = runClaim({ kind: 'decision', step_run_id: 'step', slot_id: 'slot' }, f.deps, new AbortController().signal);
  await vi.advanceTimersByTimeAsync(749);
  expect(finishDecision).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1);
  await expect(running).resolves.toBe(true);
  expect(finishDecision).toHaveBeenCalledTimes(2);
  expect(finishDecision.mock.calls[1]?.[1]).toEqual(finishDecision.mock.calls[0]?.[1]);
  expect(finishDecision.mock.calls[1]?.[2].decision).toEqual({ reason: 'The runner decision was rejected. Waiting for a person.' });
  expect(finishDecision.mock.calls[1]?.[4]).not.toEqual(finishDecision.mock.calls[0]?.[4]);
  expect(f.deps.execute).toHaveBeenCalledOnce();
});

// Release gate 2026-11-21: a report the core refuses for good (a 422 such as
// evidence.invalid_url) was retried every second forever, holding the claim
// and renewing grants for a step that could never finish.
it('sends one minimal failed report when the core refuses a report for good', async () => {
  vi.useFakeTimers();
  const f = fixture();
  const reports: unknown[] = [];
  f.deps.heartbeat = async () => {};
  f.deps.execute = async () => ({ status: 'succeeded', summary: 'done', outputs: { ok: true },
    evidence: ['http://core.example/transcript'], logs: ['excerpt'] });
  f.deps.report = async (_step, _claim, outcome) => {
    reports.push(outcome);
    if (reports.length === 1) throw new CoreError(422, 'evidence.invalid_url');
  };
  const running = f.run();
  await vi.advanceTimersByTimeAsync(5000);
  await expect(running).resolves.toBe(true);
  expect(reports).toHaveLength(2);
  expect(reports[1]).toEqual({ status: 'failed', summary: 'report_rejected:evidence.invalid_url',
    outputs: {}, evidence: [], logs: [] });
});

it('stops when the minimal failed report is refused too', async () => {
  vi.useFakeTimers();
  const f = fixture();
  f.deps.heartbeat = async () => {};
  f.deps.execute = async () => ({ status: 'failed', outputs: {}, evidence: [], logs: [] });
  const report = vi.fn(async () => { throw new CoreError(422, 'request.invalid'); });
  f.deps.report = report;
  const running = f.run();
  await vi.advanceTimersByTimeAsync(5000);
  await expect(running).resolves.toBe(false);
  expect(report).toHaveBeenCalledTimes(2);
  expect(f.calls).toContain('slot.release');
});
