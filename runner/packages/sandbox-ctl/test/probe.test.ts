import { expect, it, vi } from 'vitest';
import { enforceProbe } from '../src/probe.ts';
import { meterSandbox } from '../src/meter.ts';

it('successful_probe_stops_sandbox_and_alerts', async () => {
  const destroy = vi.fn(async () => undefined);
  const alert = vi.fn(async () => undefined);
  const exec = vi.fn(async () => ({ exit_code: 20, stdout: '', stderr: '' }));
  await expect(enforceProbe(exec, destroy, alert)).rejects.toMatchObject({ code: 'sandbox.probe_succeeded' });
  expect(destroy).toHaveBeenCalledBefore(alert);
});

it('a failed probe harness never admits a sandbox', async () => {
  const destroy = vi.fn(async () => undefined);
  const alert = vi.fn(async () => undefined);
  const exec = vi.fn(async () => ({ exit_code: 1, stdout: '', stderr: '' }));
  await expect(enforceProbe(exec, destroy, alert)).rejects.toMatchObject({ code: 'sandbox.probe_failed' });
  expect(destroy).toHaveBeenCalledOnce();
  expect(alert).not.toHaveBeenCalled();
});

it('meters_vcpu_and_memory_seconds', () => {
  expect(meterSandbox('medium', { vcpu_per_second_usd: 0.001, gib_per_second_usd: 0.002 }, 2000))
    .toEqual({ amount: 0.01, usage: { vcpu_seconds: 2, gib_seconds: 4 } });
});

it('probes the bridge gateway, a public address, external DNS and seccomp', async () => {
  const exec = vi.fn<Parameters<typeof enforceProbe>[0]>(async () => ({ exit_code: 0, stdout: '', stderr: '' }));
  const noop = async () => undefined;
  await enforceProbe(exec, noop, noop, '10.203.254.1');
  const argv = exec.mock.calls[0]?.[0].argv;
  expect(argv).toContain('10.203.254.1');
  expect(argv?.[2]).toContain('1.1.1.1');
  expect(argv?.[2]).toContain('resolve4');
  expect(argv?.[2]).toContain('Seccomp');
});

it('refuses old Docker Engines or a daemon without confined seccomp', async () => {
  const { requireDockerIsolation } = await import('../src/main.ts');
  const engine = (Version: string, SecurityOptions: string[]) => ({
    version: async () => ({ Version }), info: async () => ({ SecurityOptions }),
  }) as Parameters<typeof requireDockerIsolation>[0];
  await expect(requireDockerIsolation(engine('24.0.9', ['name=seccomp,profile=builtin']))).rejects.toThrow('25');
  await expect(requireDockerIsolation(engine('27.5.1', ['name=seccomp,profile=unconfined']))).rejects.toThrow('seccomp');
  await expect(requireDockerIsolation(engine('27.5.1', []))).rejects.toThrow('seccomp');
  await expect(requireDockerIsolation(engine('25.0.0', ['name=seccomp,profile=builtin']))).resolves.toBeUndefined();
});

it('a missing live container settles metering and removes its listener before returning gone', async () => {
  const { sandboxIdForClaim } = await import('@runbook/runner-shared/capability');
  const { HttpError } = await import('@runbook/runner-shared/http');
  const { sandboxService } = await import('../src/server.ts');
  const claims = { sub: 'step', runner_id: 'runner', claim_id: 'claim', generation: 1, exp: Date.now() / 1000 + 600 };
  const id = sandboxIdForClaim(claims);
  const listener = { proxy_url: 'http://10.2.0.2:8888', ca_pem: 'test-ca', listener_secret: 'test' };
  const backend = {
    image: 'image',
    allocateNetwork: async () => ({ id: 'network', gatewayAddress: '10.2.0.2', bridgeGateway: '10.2.0.1' }),
    start: async (_id: string, network: unknown) => ({ id, network, containerId: 'container', listener }),
    destroy: vi.fn(async () => undefined),
    exec: vi.fn().mockResolvedValueOnce({ exit_code: 0 }).mockRejectedValueOnce(new HttpError(410, 'sandbox.gone', 'gone')),
  };
  const calls: { url: string; method: string; body: Record<string, unknown> }[] = [];
  vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
    const body = JSON.parse(String(init.body)) as Record<string, unknown>;
    calls.push({ url, method: init.method!, body });
    if (url.endsWith('/compute/rates')) return Response.json({ vcpu_per_second_usd: 0, gib_per_second_usd: 0, timeout_s: 60 });
    if (url.endsWith('/ledger/reserve')) return Response.json({ reservation_id: 'reservation' });
    if (url.endsWith('/gateway/internal/sandboxes')) return Response.json(listener);
    return new Response(null, { status: 204 });
  });
  const service = sandboxService(backend as unknown as Parameters<typeof sandboxService>[0], {
    role: 'sandbox-ctl', listen: { host: '', port: 0 }, urls: { api: 'https://api.example', gateway: 'https://example.test/gateway' },
    inboundTokens: {}, outboundTokens: {},
  }, { verifyCapability: async () => claims as Awaited<ReturnType<Parameters<typeof sandboxService>[2]['verifyCapability']>> });
  try {
    const create = service.routes.find(({ path }) => path === '/internal/sandboxes')!;
    const exec = service.routes.find(({ path }) => path.endsWith('/exec'))!;
    expect(await create.handle({ caller: 'worker', params: {}, body: { capability: 'test', image: 'image', size: 'small' } }))
      .toMatchObject({ status: 201 });
    await expect(exec.handle({ caller: 'worker', params: { id }, body: {},
      request: { method: 'POST', headers: { 'x-runbook-capability': 'test' } } }))
      .rejects.toMatchObject({ status: 410, code: 'sandbox.gone' });
    expect(service.live.size).toBe(0);
    expect(backend.destroy).toHaveBeenCalledOnce();
    expect(calls).toEqual(expect.arrayContaining([
      expect.objectContaining({ url: 'https://api.example/internal/ledger/settle' }),
      expect.objectContaining({ url: `https://api.example/internal/sandboxes/${id}/meter`,
        body: expect.objectContaining({ phase: 'stopped' }) }),
      expect.objectContaining({ method: 'DELETE', url: `https://example.test/gateway/internal/sandboxes/${id}` }),
    ]));
  } finally {
    await Promise.allSettled([...service.live.values()].map((entry) => service.destroy(entry)));
    vi.unstubAllGlobals();
  }
});
