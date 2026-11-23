import { expect, it, vi } from 'vitest';
import { createApiServer } from '../src/server.ts';

it('requires a configured metrics bearer token', async () => {
  const verifier = { verifyAssertion: vi.fn() };
  const collect = vi.fn(async () => 'runner_running 2\n');
  const server = createApiServer(verifier, async () => ({}), undefined, collect, 'metrics-token');
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('missing server address');
    const url = `http://127.0.0.1:${address.port}/metrics`;
    expect((await fetch(url)).status).toBe(401);
    expect((await fetch(url, { headers: { authorization: 'Bearer incorrect' } })).status).toBe(401);
    expect(collect).not.toHaveBeenCalled();
    const response = await fetch(url, { headers: { authorization: 'Bearer metrics-token' } });
    expect(response.status).toBe(200);
    expect(await response.text()).toBe('runner_running 2\n');
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});

it('does not dispatch internal routes on the public listener', async () => {
  const internal = vi.fn((_req, res) => { res.writeHead(204).end(); });
  const server = createApiServer({ verifyAssertion: vi.fn() }, async () => ({}), internal);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('missing server address');
    expect((await fetch(`http://127.0.0.1:${address.port}/internal/ledger/reserve`, { method: 'POST' })).status).toBe(404);
    expect(internal).not.toHaveBeenCalled();
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});
