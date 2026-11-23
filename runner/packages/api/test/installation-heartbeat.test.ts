import { expect, it, vi } from 'vitest';
import type pg from 'pg';
import { sendInstallationHeartbeat } from '../src/installation-heartbeat.ts';

it('reports installed runner capacity and leases, marking an unreachable worker offline', async () => {
  const query = vi.fn().mockResolvedValue({ rows: [{ running: 1 }] });
  const fetcher = vi.fn<typeof fetch>()
    .mockResolvedValueOnce(new Response(null, { status: 404 }))
    .mockResolvedValueOnce(new Response(null, { status: 204 }))
    .mockRejectedValueOnce(new Error('worker unreachable'))
    .mockResolvedValueOnce(new Response(null, { status: 204 }));
  for (const status of ['ready', 'offline']) {
    await sendInstallationHeartbeat({
      coreUrl: 'http://core', installId: 'ri_test', token: 'test-token', runnerId: 'runner_test',
      workerUrl: 'http://worker', capacity: 2, pool: { query } as unknown as pg.Pool, fetch: fetcher,
    });
    expect(fetcher).toHaveBeenLastCalledWith('http://core/v1/runner-installs/ri_test/heartbeat',
      expect.objectContaining({
        method: 'POST',
        headers: expect.objectContaining({ authorization: 'Bearer test-token' }),
        body: JSON.stringify({ runners: [{ id: 'runner_test', status, capacity: 2, running: 1 }] }),
      }));
  }
  expect(query).toHaveBeenCalledWith(expect.objectContaining({ values: ['runner_test'] }));
});
