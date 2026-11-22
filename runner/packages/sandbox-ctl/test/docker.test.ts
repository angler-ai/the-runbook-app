import { randomUUID } from 'node:crypto';
import Docker from 'dockerode';
import { expect, it, vi } from 'vitest';
import { DockerSandboxBackend } from '../src/docker.ts';

const docker = new Docker({ socketPath: process.env.RUNNER_TEST_DOCKER_SOCKET ?? process.env.DOCKER_HOST?.replace(/^unix:\/\//, '') ?? '/var/run/docker.sock' });
const available = await docker.ping().then(() => true, () => false);
if (!available) process.stderr.write('Docker is unreachable; skipping sandbox Docker integration tests.\n');

it.skipIf(!available)('uses the hardened template, denies direct egress and confines file access', async () => {
  const image = process.env.RUNNER_TEST_SANDBOX_IMAGE ?? 'runbook-sandbox:1';
  const backend = new DockerSandboxBackend(docker, image);
  const id = `rb-test-${randomUUID()}`;
  const network = await backend.allocateNetwork(id);
  try {
    const sandbox = await backend.start(id, network, 'small', {
      proxy_url: 'http://192.0.2.1:8888/', ca_pem: 'test-ca', listener_secret: 'test-listener-password',
    });
    const { HostConfig: host, Config: config } = await docker.getContainer(sandbox.containerId).inspect();
    expect(host).toMatchObject({ ReadonlyRootfs: true, CapDrop: ['ALL'], Memory: 1073741824,
      NanoCpus: 500000000, PidsLimit: 128, SecurityOpt: ['no-new-privileges', 'seccomp=builtin'] });
    expect(config.User).toBe('10001:10001');
    expect((await docker.getNetwork(network.id).inspect()).Internal).toBe(true);
    const run = (script: string) => backend.exec(sandbox, { argv: ['/usr/local/bin/node', '-e', script], cwd: '/workspace', timeout_s: 3 });
    // A direct route would appear as a default route in this private network.
    expect((await run('process.stdout.write(require("fs").readFileSync("/proc/net/route"))')).stdout)
      .not.toMatch(/^\S+\s+00000000\s/m);
    const tooling = await backend.exec(sandbox, { argv: ['/bin/bash', '-c',
      'set -e; command -v git curl jq python3 rg unzip ps; python3 -m pip --version; python3 -m venv /workspace/venv; /workspace/venv/bin/python -m pip --version; touch /tmp/test; printf "#!/bin/sh\\necho script-ok\\n" > ./script; chmod +x ./script; ./script'],
    cwd: '/workspace', timeout_s: 12 });
    expect(tooling).toMatchObject({ exit_code: 0, stdout: expect.stringContaining('script-ok') });
    await backend.write(sandbox, 'note', 'example');
    expect(await backend.read(sandbox, 'note')).toEqual({ content: 'example', redacted: false });
    await run('require("fs").symlinkSync("/etc/passwd", "/workspace/escape")');
    await expect(backend.read(sandbox, 'escape')).rejects.toThrow('workspace');
    await expect(backend.write(sandbox, '../escaped', 'bad')).rejects.toThrow('workspace');
    const result = await run('process.stdout.write(process.env.HTTP_PROXY)');
    expect(result.stdout).not.toContain('test-listener-password');
    expect((await run("process.stdout.write('x'.repeat(300000))")).stdout).toMatch(/^x+\n\[TRUNCATED\]$/);
    await expect(backend.exec(sandbox, { argv: ['/usr/local/bin/node', '-e', 'setInterval(() => {}, 1000)'],
      cwd: '/workspace', timeout_s: 0.1 })).rejects.toMatchObject({ code: 'sandbox.exec_timeout' });
    expect(await backend.read(sandbox, 'note')).toEqual({ content: 'example', redacted: false });
    await docker.getContainer(sandbox.containerId).remove({ force: true });
    await expect(run('')).rejects.toMatchObject({ code: 'sandbox.gone' });
  } finally {
    await backend.destroy(id, network);
  }
});

it('worker_never_gets_docker_socket', async () => {
  const { loadConfig } = await import('@runbook/runner-shared/config');
  const token = 'q'.repeat(32);
  const env = {
    RUNNER_ROLE: 'worker', RUNNER_ID: 'runner', RUNNER_GROUP_ID: 'group', CORE_URL: 'https://core.example',
    CORE_CLAIM_TOKEN: token, RUNNER_DATABASE_URL: 'postgres://localhost/test',
    API_URL: 'https://api.example', VAULT_URL: 'https://vault.example',
    GATEWAY_URL: 'https://gateway.example', SANDBOX_CTL_URL: 'https://sand.example',
    RUNNER_TOKEN_API_WORKER: token, RUNNER_TOKEN_WORKER_API: token, RUNNER_TOKEN_WORKER_VAULT: token,
    RUNNER_TOKEN_WORKER_GATEWAY: token, RUNNER_TOKEN_WORKER_SANDBOXCTL: token,
  };
  expect(loadConfig('worker', env).dockerSocket).toBeUndefined();
  expect(() => loadConfig('worker', { ...env, RUNNER_DOCKER_SOCKET: '/var/run/docker.sock' })).toThrow('only sandbox-ctl');
  const { sandboxService } = await import('../src/server.ts');
  const backend = new DockerSandboxBackend(docker, 'image');
  const { routes } = sandboxService(backend, loadConfig('worker', env), {
    verifyCapability: async () => { throw new Error('the verifier should not see a missing token'); },
  });
  const create = routes.find(({ path }) => path === '/internal/sandboxes')!;
  await expect(create.handle({ caller: 'worker', params: {}, body: { image: 'image', size: 'small' } }))
    .rejects.toMatchObject({ status: 401, code: 'capability.invalid' });
});

it.skipIf(!available)('sandbox_cannot_reach_metadata directly or through the listener', async () => {
  const { enforceProbe } = await import('../src/probe.ts');
  const image = process.env.RUNNER_TEST_SANDBOX_IMAGE ?? 'runbook-sandbox:1';
  const name = `rb-test-gateway-${randomUUID()}`;
  const gateway = await docker.createContainer({
    name, Image: image, Entrypoint: ['/usr/local/bin/node', '-e'],
    Cmd: [`require('http').createServer().on('connect', (req, socket) => {
      console.log(req.url);
      socket.end('HTTP/1.1 403 Forbidden\\r\\nConnection: close\\r\\n\\r\\n');
    }).listen(8888)`],
  });
  const backend = new DockerSandboxBackend(docker, image, name);
  const id = `rb-test-${randomUUID()}`;
  try {
    await gateway.start();
    const network = await backend.allocateNetwork(id);
    try {
      const sandbox = await backend.start(id, network, 'small', {
        proxy_url: `http://${network.gatewayAddress}:8888/`, ca_pem: 'test-ca', listener_secret: 'test-password',
      });
      const fail = async () => { throw new Error('probe unexpectedly refused a sandbox'); };
      await enforceProbe((request) => backend.exec(sandbox, request), fail, fail, network.bridgeGateway);
      const logs = (await gateway.logs({ stdout: true, stderr: false })).toString();
      for (const host of ['169.254.169.254', 'metadata.google.internal', '169.254.170.2', '169.254.170.23', '[fd00:ec2::254]', '1.1.1.1', network.bridgeGateway!]) {
        expect(logs).toContain(`${host}:80`);
        expect(logs).toContain(`${host}:443`);
      }
    } finally {
      await backend.destroy(id, network);
    }
  } finally {
    await gateway.remove({ force: true, v: true });
  }
}, 10_000);

it.skipIf(!available)('orphan cleanup removes only resources in its ownership namespace', async () => {
  const image = process.env.RUNNER_TEST_SANDBOX_IMAGE ?? 'runbook-sandbox:1';
  const id = `rb-test-orphan-${randomUUID()}`;
  const backend = new DockerSandboxBackend(docker, image, undefined, undefined, `runbook.orphan-test.${id}`);
  const other = new DockerSandboxBackend(docker, image, undefined, undefined, `runbook.orphan-test.other.${id}`);
  const listener = { proxy_url: 'http://192.0.2.1:8888/', ca_pem: 'test-ca', listener_secret: 'test-password' };
  const network = await backend.allocateNetwork(id);
  try {
    const otherId = `${id}-other`;
    const otherNetwork = await other.allocateNetwork(otherId);
    try {
      const sandbox = await backend.start(id, network, 'small', listener);
      const active = await other.start(otherId, otherNetwork, 'small', listener);
      await backend.removeOrphans();
      await expect(docker.getContainer(sandbox.containerId).inspect()).rejects.toMatchObject({ statusCode: 404 });
      await expect(docker.getNetwork(network.id).inspect()).rejects.toMatchObject({ statusCode: 404 });
      await expect(docker.getContainer(active.containerId).inspect()).resolves.toMatchObject({ Id: active.containerId });
      await expect(docker.getNetwork(otherNetwork.id).inspect()).resolves.toMatchObject({ Id: otherNetwork.id });
    } finally {
      await other.destroy(otherId, otherNetwork);
    }
  } finally {
    await backend.destroy(id, network);
  }
});

it.skipIf(!available)('allocates /29 subnets, detects exhaustion, and reuses freed space', async () => {
  const backend = new DockerSandboxBackend(docker, 'unused', undefined, '10.203.254.0/29');
  const id = `rb-test-${randomUUID()}`;
  const first = await backend.allocateNetwork(id);
  try {
    expect((await docker.getNetwork(first.id).inspect()).IPAM?.Config).toEqual([
      expect.objectContaining({ Subnet: '10.203.254.0/29', Gateway: '10.203.254.1' }),
    ]);
    await expect(backend.allocateNetwork(`${id}-second`)).rejects.toMatchObject({ code: 'sandbox.subnet_exhausted' });
  } finally {
    await backend.destroy(id, first);
  }
  const reused = await backend.allocateNetwork(id);
  await backend.destroy(id, reused);
});

it.skipIf(!available)('entrypoint ends on its own after the step timeout and margin', async () => {
  const image = process.env.RUNNER_TEST_SANDBOX_IMAGE ?? 'runbook-sandbox:1';
  const backend = new DockerSandboxBackend(docker, image);
  const id = `rb-test-${randomUUID()}`;
  const network = await backend.allocateNetwork(id);
  try {
    const sandbox = await backend.start(id, network, 'small', {
      proxy_url: 'http://192.0.2.1:8888/', ca_pem: 'test-ca', listener_secret: 'test-password',
    }, 1);
    const result = await docker.getContainer(sandbox.containerId).wait();
    expect(result).toMatchObject({ StatusCode: 0 });
  } finally {
    await backend.destroy(id, network);
  }
}, 8000);

it('reports a clear startup failure for an image missing Bash or ripgrep', async () => {
  const createContainer = vi.fn(async () => ({ id: 'container', start: vi.fn() }));
  const backend = new DockerSandboxBackend({ createContainer } as unknown as Docker, 'bad-image');
  const exec = vi.spyOn(backend, 'exec').mockResolvedValue({ exit_code: 1, stdout: '', stderr: '' });
  await expect(backend.start('fixture', { id: 'network' }, 'small', {
    proxy_url: 'http://192.0.2.1:8888/', ca_pem: 'fixture', listener_secret: 'fixture',
  })).rejects.toMatchObject({ code: 'sandbox.missing_tools', message: expect.stringMatching(/Bash and ripgrep/) });
  expect(exec).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({
    argv: ['/bin/sh', '-c', expect.stringMatching(/command -v bash.*command -v rg/)],
  }));
});

it('names the missing sandbox image in startup errors', async () => {
  const docker = { createContainer: vi.fn().mockRejectedValue({ statusCode: 404 }) } as unknown as Docker;
  const backend = new DockerSandboxBackend(docker, 'runbook-sandbox:fixture');
  await expect(backend.start('fixture', { id: 'network' }, 'small', {
    proxy_url: 'http://192.0.2.1:8888/', ca_pem: 'fixture', listener_secret: 'fixture',
  })).rejects.toMatchObject({ code: 'sandbox.image_missing', message: expect.stringContaining('runbook-sandbox:fixture') });
});
