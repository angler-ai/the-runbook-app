import { createServer, type Socket } from 'node:net';
import { expect, it } from 'vitest';
import { openPool } from '../src/db.ts';

it('bounds a connection attempt even when a proxy accepts TCP but never answers', async () => {
  const sockets = new Set<Socket>();
  const proxy = createServer((socket) => { sockets.add(socket); socket.once('close', () => sockets.delete(socket)); });
  await new Promise<void>((resolve) => proxy.listen(0, '127.0.0.1', resolve));
  const address = proxy.address();
  if (!address || typeof address === 'string') throw new Error('expected TCP listener');
  const pool = openPool('api', `postgres://unused@127.0.0.1:${address.port}/unused`);
  const emergency = setTimeout(() => {
    for (const socket of sockets) socket.destroy(new Error('fixture_deadline'));
  }, 6_000);
  // The fixture closes hanging sockets even against a pool with no timeout.
  proxy.on('connection', (socket) => socket.on('error', () => undefined));
  try {
    const started = performance.now();
    await expect(pool.connect()).rejects.toThrow(/timeout/i);
    expect(performance.now() - started).toBeLessThan(5_500);
    expect(pool.totalCount).toBe(0);
  } finally {
    clearTimeout(emergency);
    for (const socket of sockets) socket.destroy();
    await pool.end();
    proxy.close();
  }
});
