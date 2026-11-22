import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import http from 'node:http';
import https from 'node:https';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import Docker from 'dockerode';
import { expect, it, vi } from 'vitest';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import { DockerSandboxBackend } from '../../sandbox-ctl/src/docker.ts';
import { MaterialStore } from '../../gateway/src/material.ts';
import { sandboxListener } from '../../gateway/src/egress/listener.ts';
import * as rules from '../../gateway/src/egress/rules.ts';

const socket = process.env.RUNNER_TEST_DOCKER_SOCKET ?? process.env.DOCKER_HOST?.replace(/^unix:\/\//, '');
const docker = new Docker({ socketPath: socket ?? '/var/run/docker.sock' });
const available = Boolean(socket) && await docker.ping().then(() => true, () => false);

it.skipIf(!available)('clones a local Git repository through the sandbox gateway TLS proxy', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'runbook-git-'));
  const id = 'rb-git-' + randomUUID();
  const image = process.env.RUNNER_TEST_SANDBOX_IMAGE ?? 'runbook-sandbox:1';
  const host = 'git.example';
  const store = new MaterialStore();
  const exp = Math.floor(Date.now() / 1000) + 60;
  const claims = { kind: 'capability', sub: id, claim_id: 'claim', generation: 1, runner_id: 'runner', exp } as GrantClaims;
  const layer = { [host]: 'read' as const };
  const policy = { layers: [layer, layer, layer], credentialLabels: { git: 'read' as const }, destructive: false };
  const backend = new DockerSandboxBackend(docker, image, id + '-gateway');
  const upstream = http.createServer((request, response) => {
    expect(request.headers['x-fixture-key']).toBe('local-git-fixture-secret');
    void readFile(join(dir, (request.url ?? '').split('?')[0]!)).then(
      (data) => response.end(data), () => response.writeHead(404).end(),
    );
  });
  try {
    const git = (...args: string[]) => execFileSync('git', args, { cwd: dir, stdio: 'pipe' });
    git('init', '-b', 'main');
    git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.com', 'commit', '--allow-empty', '-m', 'local fixture');
    git('update-server-info');
    await new Promise<void>((resolve) => upstream.listen(0, '127.0.0.1', resolve));
    const address = upstream.address();
    if (!address || typeof address === 'string') throw new Error('Missing fixture address');
    // Route only the gateway's upstream connection to the local dumb-HTTP Git server.
    vi.spyOn(rules, 'pinnedAddress').mockResolvedValue({ address: '127.0.0.1', family: 4 });
    vi.spyOn(https, 'request').mockImplementation(((options: http.RequestOptions, callback: (res: http.IncomingMessage) => void) =>
      http.request({ ...options, hostname: '127.0.0.1', port: address.port }, callback)) as typeof https.request);
    store.put({
      release_id: 'git', credential_id: 'git', step_run_id: id, claim_id: 'claim', generation: 1, runner_id: 'runner',
      expires_at: new Date(exp * 1000).toISOString(), delivery: 'proxy',
      binding: { hosts: [host], header_name: 'x-fixture-key', header_format: '{}' },
      material: { fields: { key: 'local-git-fixture-secret' } },
    });
    const listener = await sandboxListener({ claims, store, layers: policy.layers, policy: async () => policy,
      listenHost: '0.0.0.0', publicHost: '127.0.0.1' });
    try {
      const port = new URL(listener.proxy_url).port;
      const gateway = await docker.createContainer({
        Image: image, name: id + '-gateway', Entrypoint: ['/usr/local/bin/node', '-e'],
        Cmd: [`const net = require('node:net');
          net.createServer(socket => {
            const host = net.connect(${port}, 'host.docker.internal');
            host.on('error', () => socket.destroy());
            socket.on('error', () => host.destroy());
            socket.pipe(host).pipe(socket);
          }).listen(8888);`],
        HostConfig: { ExtraHosts: ['host.docker.internal:host-gateway'] },
      });
      try {
        await gateway.start();
        const network = await backend.allocateNetwork(id);
        try {
          const sandbox = await backend.start(id, network, 'small', {
            ...listener, proxy_url: `http://${network.gatewayAddress}:8888/`,
          });
          const clone = await backend.exec(sandbox, {
            argv: ['/bin/bash', '-ec', 'git clone https://git.example/.git cloned && git -C cloned log -1 --format=%s'],
            cwd: '/workspace', timeout_s: 5,
          });
          expect(clone, clone.stderr).toMatchObject({ exit_code: 0, stdout: 'local fixture\n' });
        } finally {
          await backend.destroy(id, network);
        }
      } finally {
        await gateway.remove({ force: true, v: true });
      }
    } finally {
      listener.close();
    }
  } finally {
    vi.restoreAllMocks();
    store.clear();
    upstream.close();
    await rm(dir, { force: true, recursive: true });
  }
}, 20_000);
