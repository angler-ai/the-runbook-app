import { createServer, type IncomingMessage } from 'node:http';
import { WebSocketServer } from 'ws';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import { KubernetesApi } from '../src/kubernetes-api.ts';
import { KubernetesSandboxBackend } from '../src/kubernetes.ts';

import { podTemplate } from '../src/kubernetes-template.ts';

const claims = { sub: 'sr_test', claim_id: 'claim', generation: 2 };

describe('Kubernetes sandbox template', () => {
  it('disables service-account tokens and bounds a gVisor pod by digest and step deadline', () => {
    const image = 'registry.example/sandbox@sha256:' + 'a'.repeat(64);
    const pod = podTemplate('pod', 'rb-test', claims, image, 'small', 4300);
    expect(pod.spec).toMatchObject({
      runtimeClassName: 'gvisor',
      automountServiceAccountToken: false,
      activeDeadlineSeconds: 5,
      securityContext: { runAsUser: 10001, fsGroup: 10001 },
      containers: [{ image, resources: { requests: { cpu: '500m', memory: '2Gi', 'ephemeral-storage': '1Gi' } } }],
    });
    expect(pod.spec.containers[0]!.resources.requests).toEqual(pod.spec.containers[0]!.resources.limits);
    expect(pod.metadata.annotations['runbook.dev/claim-id']).toBe('claim');
  });
  it('mounts the trusted installation helper read-only before custom image code starts', () => {
    const trustedImage = 'registry.example/installation@sha256:' + 'a'.repeat(64);
    const customImage = 'registry.example/custom@sha256:' + 'b'.repeat(64);
    const options = { trustedImage };
    const pod = podTemplate('pod', 'rb-test', claims, customImage, 'small', 4300, options);
    expect(pod.spec).toMatchObject({
      initContainers: [{ image: trustedImage, volumeMounts: [{ name: 'trusted', mountPath: '/copy' }] }],
      containers: [{ image: customImage, command: ['/runbook-trusted/helper', 'idle'],
        volumeMounts: expect.arrayContaining([{ name: 'trusted', mountPath: '/runbook-trusted', readOnly: true }]) }],
    });
  });

});

const image = 'registry.example/sandbox@sha256:' + 'a'.repeat(64);
const id = 'rb-' + 'b'.repeat(64);
const grant = { ...claims } as GrantClaims;
const listener = { proxy_url: 'http://10.0.0.2:20000', listener_secret: 'opaque-credential', ca_pem: 'test-ca' };

async function body(req: IncomingMessage) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return JSON.parse(Buffer.concat(chunks).toString());
}

describe('Kubernetes sandbox against the API', () => {
  const accepted = () => ({ phase: 'Running', containerStatuses: [{ name: 'sandbox', state: { running: {} } }] });
  let pod: (ReturnType<typeof podTemplate> & { metadata: { uid: string } }) | undefined;
  let phase: ReturnType<typeof accepted> | { phase: string; reason?: string };
  let admitted: ReturnType<typeof podTemplate> | undefined;
  let upgradeCount: number;
  let execCommands: string[][];
  let deleteCount: number;
  let stallExec: boolean;
  let attest: boolean;
  let initialization: Pick<typeof listener, 'ca_pem'> | undefined;
  let workspace: Record<string, string>;
  let backend: KubernetesSandboxBackend;
  let close: () => Promise<void>;

  beforeEach(async () => {
    pod = admitted = undefined;
    initialization = undefined;
    upgradeCount = deleteCount = 0;
    execCommands = [];
    stallExec = false;
    attest = true;
    workspace = {};
    phase = accepted();
    const api = createServer(async (req, res) => {
      const path = new URL(req.url!, 'http://localhost');
      let answer;
      if (req.method === 'POST') {
        admitted = await body(req);
        pod = { ...admitted!, metadata: { ...admitted!.metadata, uid: 'owned-uid' } };
        pod.spec.containers[0]!.resources = {
          requests: { cpu: '750m', memory: '2Gi', 'ephemeral-storage': '1Gi' },
          limits: { cpu: '750m', memory: '2Gi', 'ephemeral-storage': '1Gi' },
        };
        answer = pod;
      } else if (req.method === 'DELETE') {
        expect((await body(req)).preconditions).toEqual({ uid: 'owned-uid' });
        deleteCount++;
        pod = undefined;
        answer = {};
      } else if (path.searchParams.get('labelSelector') === 'runbook.dev/sandbox=true') {
        answer = { items: pod ? [pod] : [] };
      } else if (pod && path.pathname.endsWith('/' + pod.metadata.name)) {
        answer = { ...pod, status: phase };
      } else {
        res.statusCode = 404;
        answer = {};
      }
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify(answer));
    });
    const exec = new WebSocketServer({ noServer: true });
    api.on('upgrade', (req, socket, head) => {
      upgradeCount++;
      const url = new URL(req.url!, 'http://localhost');
      const argv = url.searchParams.getAll('command');
      execCommands.push(argv);
      expect(url.searchParams.get('tty')).toBe('false');
      expect(url.searchParams.get('stdin')).toBe('true');
      expect(url.searchParams.get('container')).toBe('sandbox');
      expect(url.search).not.toContain(listener.listener_secret);
      exec.handleUpgrade(req, socket, head, (ws) => {
        ws.once('message', (raw: Buffer) => {
          const payload = JSON.parse(raw.subarray(5).toString());
          expect(raw.readUInt32BE(1)).toBe(raw.length - 5);
          let output = '';
          let code = 0;
          if (payload.argv) {
            expect(payload.proxy_url).toBe(listener.proxy_url);
            expect(payload.listener_secret).toBe(listener.listener_secret);
          }
          if (argv.includes('attest') || argv.some((part) => part.includes('Starting gVisor'))) code = attest ? 0 : 1;
          else if (argv.includes('init')) initialization = payload;
          else if (argv.includes('probe-kube')) code = 0;
          else if (stallExec) return;
          else if (payload.argv[3] === 'write') workspace[payload.argv[4]] = payload.stdin;
          else if (payload.argv[3] === 'read') output = workspace[payload.argv[4]] ?? '';
          else if (payload.argv[0] === 'timeout') code = 124;
          else if (payload.argv[0] === 'verbose') output = 'x'.repeat(350_000);
          else { output = 'stdout'; code = 7; }
          if (output) ws.send(Buffer.concat([Buffer.from([1]), Buffer.from(output)]));
          const status = code ? { status: 'Failure', reason: 'NonZeroExitCode', details: { causes: [
            { reason: 'ExitCode', message: String(code) },
          ] } } : { status: 'Success' };
          ws.send(Buffer.concat([Buffer.from([3]), Buffer.from(JSON.stringify(status))]));
        });
      });
    });
    await new Promise<void>((resolve) => api.listen(0, '127.0.0.1', resolve));
    const address = api.address();
    if (!address || typeof address === 'string') throw new Error('expected fake API address');
    backend = new KubernetesSandboxBackend(new KubernetesApi(`http://127.0.0.1:${address.port}`, 'sandbox'), image, 500, {
      namePrefix: 'runbook-test2-', serviceAccountName: 'runbook-test2-sandbox',
      labels: { 'runbook-test': 'true', 'runbook.dev/sandbox': 'false' },
    });
    close = async () => {
      for (const client of exec.clients) client.terminate();
      exec.close();
      await new Promise<void>((resolve) => { api.close(() => resolve()); api.closeAllConnections(); });
    };
  });

  afterEach(async () => close());

  async function start() {
    return backend.start(id, await backend.allocateNetwork(id), 'small', async () => listener, 90_000, grant);
  }
  const command = (argv: string[]) => ({ argv, cwd: '/workspace', timeout_s: 1 });

  it('submits the install-specific pod name, service account and labels', async () => {
    await start();
    expect(admitted?.metadata.name).toMatch(/^runbook-test2-rb-[0-9a-f-]{36}$/);
    expect(admitted?.metadata.labels).toEqual({ 'runbook-test': 'true', 'runbook.dev/sandbox': 'true' });
    expect(admitted?.spec.serviceAccountName).toBe('runbook-test2-sandbox');
  });

  it('attests gVisor before provisioning the listener or sending its credentials', async () => {
    attest = false;
    let provisioned = false;
    const network = await backend.allocateNetwork(id);
    await expect(backend.start(id, network, 'small', async () => {
      provisioned = true;
      return listener;
    }, 90_000, grant)).rejects.toMatchObject({ code: 'sandbox.runtime_attestation_failed' });
    expect(provisioned).toBe(false);
    expect(initialization).toBeUndefined();
  });

  it('ignores malformed generation annotations and continues reaping', async () => {
    await start();
    pod!.metadata.annotations['runbook.dev/generation'] = '02';
    await expect(backend.removeOrphans()).resolves.toEqual([]);
    expect(deleteCount).toBe(0);
  });

  it('attests the running custom sandbox with the read-only helper', async () => {
    const customImage = 'registry.example/custom@sha256:' + 'b'.repeat(64);
    const network = await backend.allocateNetwork(id);
    const sandbox = await backend.start(id, network, 'small', async () => listener, 4800, grant,
      undefined, undefined, customImage);
    try {
      expect(await backend.customProbe(sandbox)).toMatchObject({ exit_code: 0 });
      expect(execCommands).toEqual([
        ['/runbook-trusted/helper', 'attest'], ['/runbook-trusted/helper', 'init'],
        ['/runbook-trusted/helper', 'probe-kube'],
      ]);
    } finally {
      await backend.destroy(id, network);
    }
  });

  it('creates by digest, transfers credentials only over stdin, and reads admitted metering values', async () => {
    const sandbox = await start();
    expect(sandbox.resources).toEqual({ vcpu: 0.75, gib: 2 });
    expect(admitted!.spec.runtimeClassName).toBe('gvisor');
    expect(JSON.stringify(admitted)).not.toContain(listener.listener_secret);
    expect(initialization).toEqual({ ca_pem: listener.ca_pem });
  });

  it('reports exit status and caps output', async () => {
    const sandbox = await start();
    expect(await backend.exec(sandbox, command(['test']))).toMatchObject({ exit_code: 7, stdout: 'stdout' });
    const output = await backend.exec(sandbox, command(['verbose']));
    expect(output.truncated).toBe(true);
    expect(Buffer.byteLength(output.stdout) + Buffer.byteLength(output.stderr)).toBeLessThanOrEqual(256 * 1024);
  });

  it('uses the workspace file helper over exec and rejects traversal', async () => {
    const sandbox = await start();
    await backend.write(sandbox, 'work/one', 'sample');
    expect(await backend.read(sandbox, 'work/one')).toEqual({ content: 'sample', redacted: false });
    await expect(backend.read(sandbox, '../one')).rejects.toMatchObject({ code: 'sandbox.invalid_file' });
  });

  it('reports child timeout and terminates the pod when the caller cancels exec', async () => {
    const sandbox = await start();
    await expect(backend.exec(sandbox, command(['timeout']))).rejects.toMatchObject({ code: 'sandbox.exec_timeout' });
    expect(await backend.exec(sandbox, { ...command(['timeout']), return_partial: true }))
      .toMatchObject({ exit_code: 124, timed_out: true });
    stallExec = true;
    const abort = new AbortController();
    const running = backend.exec(sandbox, command(['test']), abort.signal);
    setTimeout(() => abort.abort(), 20);
    await expect(running).rejects.toMatchObject({ code: 'sandbox.gone' });
    expect(deleteCount).toBe(1);
  });

  it('fails a pending pod at the creation deadline and removes it', async () => {
    phase = { phase: 'Pending' };
    await expect(start()).rejects.toMatchObject({ code: 'sandbox.unavailable' });
    expect(deleteCount).toBe(1);
  });

  it('fails an evicted attempt before opening a new exec connection', async () => {
    const sandbox = await start();
    phase = { phase: 'Failed', reason: 'Evicted' };
    await expect(backend.exec(sandbox, command(['test']))).rejects.toMatchObject({ code: 'sandbox.gone' });
    expect(upgradeCount).toBe(2);
  });

  it('refuses exec and deletion against a different UID or claim', async () => {
    const sandbox = await start();
    const network = sandbox.network;
    pod!.metadata.uid = 'replacement';
    await expect(backend.exec(sandbox, command(['test']))).rejects.toMatchObject({ code: 'sandbox.identity_mismatch' });
    await expect(backend.destroy(id, network)).rejects.toMatchObject({ code: 'sandbox.identity_mismatch' });
    pod!.metadata.uid = 'owned-uid';
    pod!.metadata.annotations['runbook.dev/claim-id'] = 'different-claim';
    await expect(backend.destroy(id, network)).rejects.toMatchObject({ code: 'sandbox.identity_mismatch' });
    expect(upgradeCount).toBe(2);
    expect(deleteCount).toBe(0);
  });

  it('reaps a duplicate sandbox ID without releasing the live UID’s slot', async () => {
    await start();
    expect(await backend.removeOrphans((candidate, uid) => candidate === id && uid !== 'owned-uid')).toEqual([]);
    expect(deleteCount).toBe(1);
  });

  it('reaps annotated orphan pods and leaves currently owned pods alone', async () => {
    await start();
    expect(await backend.removeOrphans((candidate) => candidate === id)).toEqual([]);
    expect(await backend.removeOrphans()).toEqual([id]);
    expect(deleteCount).toBe(1);
  });
});
