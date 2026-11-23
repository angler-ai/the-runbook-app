import { expect, it, vi } from 'vitest';
import type { Config } from '@runbook/runner-shared/config';
import { gatewayListenerAlive, reapSandboxes } from '../src/reaper.ts';
import type { RunningSandbox } from '../src/server.ts';

it('reaper_destroys_sandbox_without_grant', async () => {
  const moved = { claims: { exp: 20 }, sandbox: { id: 'moved' } } as RunningSandbox;
  const running = { claims: { exp: 20 }, sandbox: { id: 'live' } } as RunningSandbox;
  const expired = { claims: { exp: 10 }, sandbox: { id: 'expired' } } as RunningSandbox;
  const verify = vi.fn(async (entry: RunningSandbox) => entry === running);
  const destroy = vi.fn(async () => undefined);
  await reapSandboxes([moved, running, expired], verify, destroy, 10_000);
  expect(verify.mock.calls).toEqual([[moved], [running]]);
  expect(destroy.mock.calls).toEqual(expect.arrayContaining([[moved], [expired]]));
  expect(destroy).toHaveBeenCalledTimes(2);
});

it('preserves a still-signed lease when the vault check is transiently unavailable', async () => {
  const running = { claims: { exp: 20 }, sandbox: { id: 'live' } } as RunningSandbox;
  const verify = vi.fn(async () => { throw new Error('vault temporarily unavailable'); });
  const destroy = vi.fn(async () => undefined);
  await reapSandboxes([running], verify, destroy, 19_000);
  expect(destroy).not.toHaveBeenCalled();
  await reapSandboxes([running], verify, destroy, 20_000);
  expect(destroy).toHaveBeenCalledExactlyOnceWith(running);
});

it('reaps a still-leased sandbox when its restarted gateway has no listener', async () => {
  const running = { capability: 'current-capability', claims: { exp: 20 }, sandbox: { id: 'rb-sandbox' } } as RunningSandbox;
  const config = { urls: { gateway: 'http://gateway' }, outboundTokens: { gateway: 'controller-token' } } as Config;
  const destroy = vi.fn(async () => undefined);
  const fetcher = vi.fn<typeof fetch>().mockRejectedValueOnce(new Error('restart')).mockResolvedValueOnce(new Response(null, { status: 410 }));
  const verify = (entry: RunningSandbox) => gatewayListenerAlive(config, entry, fetcher);
  await reapSandboxes([running], verify, destroy, 10_000);
  expect(destroy).not.toHaveBeenCalled();
  await reapSandboxes([running], verify, destroy, 15_000);
  expect(destroy).toHaveBeenCalledExactlyOnceWith(running);
  expect(fetcher).toHaveBeenLastCalledWith('http://gateway/internal/sandboxes/rb-sandbox/check',
    expect.objectContaining({ method: 'POST', body: JSON.stringify({ capability: running.capability }) }));
});
