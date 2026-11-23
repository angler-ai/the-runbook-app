import { once } from 'node:events';
import { request } from 'node:http';
import { createLogger } from '@runbook/runner-shared/log';
import { expect, it } from 'vitest';
import { createGatewayServer } from '../src/server.ts';

it('rejects other callers and private destinations at the OAuth CONNECT listener', async () => {
  const log = createLogger('test');
  const server = createGatewayServer({ inboundTokens: { vault: 'vault-fixture', worker: 'worker-fixture' }, log });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('test listener unavailable');
  const connect = (target: string, token: string) => new Promise<number | undefined>((resolve, reject) => {
    const outgoing = request({
      host: '127.0.0.1', port: address.port, method: 'CONNECT', path: target,
      headers: { 'proxy-authorization': `Bearer ${token}` },
    });
    outgoing.on('connect', (response, socket) => {
      socket.destroy();
      resolve(response.statusCode);
    });
    outgoing.on('error', reject);
    outgoing.end();
  });
  try {
    await expect(connect('example.com:443', 'worker-fixture')).resolves.toBe(407);
    await expect(connect('127.0.0.1:443', 'vault-fixture')).resolves.toBe(403);
    await expect(connect('[::1]:443', 'vault-fixture')).resolves.toBe(403);
    await expect(connect('example.com:80', 'vault-fixture')).resolves.toBe(403);
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});
