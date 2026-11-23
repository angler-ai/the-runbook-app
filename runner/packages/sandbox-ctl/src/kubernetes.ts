import { randomUUID } from 'node:crypto';
import { setTimeout as sleep } from 'node:timers/promises';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import type { ExecRequest, ExecResponse, SandboxListenerResponse, SandboxSize } from '@runbook/runner-shared/contracts/internal';
import { HttpError } from '@runbook/runner-shared/http';
import { loadRedaction } from '@runbook/runner-shared/redact';
import { captureEnds, OUTPUT_LIMIT, validateExec, validatePath, type DockerSandbox, type SandboxNetwork } from './docker.ts';
import { type KubernetesApi, KubernetesApiError } from './kubernetes-api.ts';
import { DIGEST_IMAGE, KUBE_HELPER, SANDBOX_CONTAINER, SANDBOX_LABEL, podTemplate, sandboxAnnotations, type KubernetesSandboxOptions } from './kubernetes-template.ts';
import { KUBERNETES_RUNTIME_PROBE } from './kubernetes-probe.ts';
import { FILE_HELPER, NODE, TRUSTED_HELPER } from './template.ts';

export const KUBERNETES_CREATE_MS = 300_000;
const unavailable = () => new HttpError(503, 'sandbox.unavailable', 'sandbox operation unavailable');
const gone = () => new HttpError(410, 'sandbox.gone', 'sandbox pod is gone');
const timedOut = () => new HttpError(408, 'sandbox.exec_timeout', 'sandbox execution timed out');
type Annotations = ReturnType<typeof sandboxAnnotations>;

interface KubernetesPod {
  metadata: {
    name: string; uid: string; annotations?: Record<string, string>; deletionTimestamp?: string;
  };
  spec?: { runtimeClassName?: string; containers?: {
    name: string; resources?: { requests?: Record<string, string> };
  }[] };
  status?: { phase?: string; reason?: string; startTime?: string; containerStatuses?: {
    name: string; state?: { running?: { startedAt?: string }; terminated?: { finishedAt?: string } };
  }[] };
}

interface Identity { name: string; uid: string; annotations: Annotations }

function admittedResources(pod: KubernetesPod) {
  const request = pod.spec?.containers?.find(({ name }) => name === SANDBOX_CONTAINER)?.resources?.requests;
  function quantity(value: string | undefined, units: Record<string, number>) {
    const parsed = /^(\d+(?:\.\d+)?)([A-Za-z]*)$/.exec(value ?? '');
    const number = parsed ? Number(parsed[1]) * (units[parsed[2]!] ?? NaN) : NaN;
    if (!(number > 0) || !Number.isFinite(number)) throw unavailable();
    return number;
  }
  return {
    vcpu: quantity(request?.cpu, { '': 1, m: 1e-3 }),
    gib: quantity(request?.memory, { '': 2 ** -30, k: 1e3 / 2 ** 30, M: 1e6 / 2 ** 30, G: 1e9 / 2 ** 30,
      Ki: 2 ** -20, Mi: 2 ** -10, Gi: 1 }),
  };
}

export class KubernetesSandboxBackend {
  private readonly identities = new WeakMap<SandboxNetwork, Identity>();

  private readonly api: KubernetesApi;
  readonly image: string;
  private readonly createMs: number;
  private readonly options: KubernetesSandboxOptions;

  constructor(api: KubernetesApi, image: string, createMs = KUBERNETES_CREATE_MS,
    options: KubernetesSandboxOptions = {}) {
    this.api = api;
    this.image = image;
    this.createMs = createMs;
    this.options = options;
    if (!DIGEST_IMAGE.test(image)) throw new Error('RUNNER_SANDBOX_IMAGE must be pinned by digest');
  }

  async allocateNetwork(id: string): Promise<SandboxNetwork> {
    // NetworkPolicy grants only the gateway listener range. The gateway
    // advertises its current pod IP, which also fences a recreated gateway.
    return { id };
  }

  async start(id: string, network: SandboxNetwork, size: SandboxSize, provision: () => Promise<SandboxListenerResponse>,
    timeoutMs: number, claims: GrantClaims, signal?: AbortSignal,
    onRunning?: (resources: NonNullable<DockerSandbox['resources']>, startedAt: number) => void, image = this.image): Promise<DockerSandbox> {
    const deadline = AbortSignal.timeout(this.createMs);
    const waiting = signal ? AbortSignal.any([deadline, signal]) : deadline;
    const name = `${this.options.namePrefix ?? ''}rb-${randomUUID()}`;
    const customImage = image !== this.image;
    const template = podTemplate(name, id, claims, image, size, timeoutMs, { ...this.options, trustedImage: this.image });
    try {
      const created = await this.api.json<KubernetesPod>('POST', '', template, undefined, waiting);
      if (created.metadata.name !== name || !created.metadata.uid) throw unavailable();
      const identity = { name, uid: created.metadata.uid, annotations: template.metadata.annotations };
      this.identities.set(network, identity);
      let startedAt: number | undefined;
      for (;;) {
        const pod = await this.checked(identity, waiting);
        if (pod.spec?.runtimeClassName !== 'gvisor') throw unavailable();
        const container = pod.status?.containerStatuses?.find(({ name }) => name === SANDBOX_CONTAINER);
        const running = pod.status?.phase === 'Running' && container?.state?.running;
        const podStartedAt = Date.parse(pod.status?.startTime ?? '');
        if (startedAt === undefined && (Number.isFinite(podStartedAt) || running)) {
          startedAt = Number.isFinite(podStartedAt) ? podStartedAt : Date.parse(container?.state?.running?.startedAt ?? '') || Date.now();
          onRunning?.(admittedResources(pod), startedAt);
        }
        if (running) {
          const resources = admittedResources(pod);
          const attestation = await this.api.exec(name, SANDBOX_CONTAINER, customImage ? [TRUSTED_HELPER, 'attest'] : KUBERNETES_RUNTIME_PROBE,
            {}, () => {}, AbortSignal.any([waiting, AbortSignal.timeout(6000)])).catch(() => 1);
          if (attestation !== 0) {
            throw new HttpError(503, 'sandbox.runtime_attestation_failed', 'sandbox runtime attestation did not complete');
          }
          const listener = await provision();
          await this.checked(identity, waiting);
          await this.api.exec(name, SANDBOX_CONTAINER, customImage ? [TRUSTED_HELPER, 'init'] : [...KUBE_HELPER, 'init'], { ca_pem: listener.ca_pem }, () => {}, waiting)
            .then((code) => { if (code !== 0) throw unavailable(); });
          return { id, containerId: identity.uid, listener, network, resources, startedAt, customImage };
        }
        // A Pending pod can wait for a cold gVisor node.
        await sleep(500, undefined, { signal: waiting });
      }
    } catch (error) {
      await this.destroy(id, network).catch(() => undefined);
      throw error instanceof HttpError ? error : unavailable();
    }
  }

  private async checked(identity: Identity, signal?: AbortSignal, deleting = false) {
    const pod = await this.api.json<KubernetesPod>('GET', identity.name, undefined, undefined, signal)
      .catch((error: unknown) => { throw error instanceof KubernetesApiError && error.status === 404 ? gone() : unavailable(); });
    if (pod.metadata.name !== identity.name || pod.metadata.uid !== identity.uid ||
        Object.entries(identity.annotations).some(([key, value]) => pod.metadata.annotations?.[key] !== value)) {
      throw new HttpError(409, 'sandbox.identity_mismatch', 'sandbox pod ownership changed');
    }
    if (!deleting && (pod.metadata.deletionTimestamp || ['Failed', 'Succeeded'].includes(pod.status?.phase ?? '') ||
        pod.status?.reason === 'Evicted')) throw gone();
    return pod;
  }

  async isRunning(sandbox: DockerSandbox): Promise<boolean> {
    const identity = this.identities.get(sandbox.network);
    if (!identity) return false;
    try {
      return (await this.checked(identity)).status?.phase === 'Running';
    } catch (error) {
      if (error instanceof HttpError && error.code === 'sandbox.gone') return false;
      throw error;
    }
  }

  private async remove(identity: Identity) {
    try {
      await this.checked(identity, undefined, true);
      await this.api.json('DELETE', identity.name, {
        apiVersion: 'v1', kind: 'DeleteOptions', gracePeriodSeconds: 1, preconditions: { uid: identity.uid },
      });
      return true;
    } catch (error) {
      if (!(error instanceof HttpError && error.code === 'sandbox.gone') &&
          !(error instanceof KubernetesApiError && error.status === 404)) throw error;
      return false;
    }
  }

  private async removeAndConfirm(identity: Identity): Promise<boolean> {
    const removed = await this.remove(identity);
    if (!removed) return false;
    const until = AbortSignal.timeout(5000);
    for (;;) {
      const pod = await this.checked(identity, until, true).catch((error: unknown) => {
        if (error instanceof HttpError && error.code === 'sandbox.gone') return undefined;
        throw error;
      });
      if (!pod || ['Failed', 'Succeeded'].includes(pod.status?.phase ?? '')) return true;
      await sleep(150, undefined, { signal: until }).catch(() => { throw unavailable(); });
    }
  }

  async destroy(id: string, network: SandboxNetwork): Promise<void> {
    const identity = this.identities.get(network);
    if (!identity) return;
    if (identity.annotations['runbook.dev/sandbox-id'] !== id) throw unavailable();
    await this.removeAndConfirm(identity);
  }

  async removeOrphans(isOwned: (id: string, uid?: string) => boolean = () => false): Promise<string[]> {
    const ids = new Set<string>();
    let token = '';
    do {
      const query = new URLSearchParams({ labelSelector: `${SANDBOX_LABEL}=true`, limit: '100' });
      if (token) query.set('continue', token);
      const { items, metadata } = await this.api.json<{ items: KubernetesPod[]; metadata?: { continue?: string } }>(
        'GET', '', undefined, query);
      for (const { metadata: { name, uid, annotations } } of items) {
        const id = annotations?.['runbook.dev/sandbox-id'];
        if (!id || !/^rb-[a-f0-9]{64}$/.test(id) || !uid || isOwned(id, uid)) continue;
        const sub = annotations?.['runbook.dev/step-run'];
        const claim = annotations?.['runbook.dev/claim-id'];
        const generation = annotations?.['runbook.dev/generation'];
        const number = Number(generation);
        if (!sub || !claim || !generation || !Number.isSafeInteger(number) || number < 1 || String(number) !== generation) continue;
        const removed = await this.removeAndConfirm({ name, uid, annotations: sandboxAnnotations(id, { sub, claim_id: claim, generation: number }) });
        if (removed && !isOwned(id)) ids.add(id);
      }
      token = metadata?.continue ?? '';
    } while (token);
    return [...ids];
  }

  async customProbe(sandbox: DockerSandbox, signal?: AbortSignal): Promise<ExecResponse> {
    const identity = this.identities.get(sandbox.network);
    if (!identity || identity.uid !== sandbox.containerId) throw gone();
    const timeout = AbortSignal.timeout(6000);
    const bounded = signal ? AbortSignal.any([signal, timeout]) : timeout;
    await this.checked(identity, bounded);
    const exit_code = await this.api.exec(identity.name, SANDBOX_CONTAINER, [TRUSTED_HELPER, 'probe-kube'],
      { proxy_url: sandbox.listener.proxy_url, listener_secret: sandbox.listener.listener_secret }, () => {}, bounded);
    return { exit_code, stdout: '', stderr: '' };
  }

  async exec(sandbox: DockerSandbox, request: ExecRequest, signal?: AbortSignal, setup = false): Promise<ExecResponse> {
    validateExec(request, setup);
    const identity = this.identities.get(sandbox.network);
    if (!identity || identity.uid !== sandbox.containerId) throw gone();
    const watchdog = AbortSignal.timeout(Math.round(request.timeout_s * 1000) + 4000);
    const bounded = signal ? AbortSignal.any([signal, watchdog]) : watchdog;
    const redaction = await loadRedaction();
    const safe = () => captureEnds(redaction.stream([sandbox.listener.listener_secret]));
    const out = safe();
    const err = safe();
    let exit: number;
    try {
      await this.checked(identity, bounded);
      const argv = !setup && sandbox.env && Object.keys(sandbox.env).length
        ? ['/usr/bin/env', ...Object.entries(sandbox.env).map(([name, value]) => `${name}=${value}`), ...request.argv]
        : request.argv;
      exit = await this.api.exec(identity.name, SANDBOX_CONTAINER, setup ? [TRUSTED_HELPER, 'setup-kube'] : KUBE_HELPER,
        { ...request, argv, ...(setup && { env: sandbox.env }),
          proxy_url: sandbox.listener.proxy_url, listener_secret: sandbox.listener.listener_secret },
        (channel, bytes) => (channel === 1 ? out : err).sink.write(bytes), bounded);
      const timed_out = exit === 124 || exit === 137;
      if (timed_out && !request.return_partial) throw timedOut();
      const stdout = out.finish();
      const stderr = err.finish();
      const text = stdout.render(OUTPUT_LIMIT - Math.min(stderr.size, OUTPUT_LIMIT / 2));
      return { exit_code: exit, stdout: text, stderr: stderr.render(OUTPUT_LIMIT - Buffer.byteLength(text)),
        ...(timed_out && { timed_out }), ...(stdout.redacted && { stdout_redacted: true }),
        ...(stdout.size + stderr.size > OUTPUT_LIMIT && { truncated: true }) };
    } catch (error) {
      if (bounded.aborted) {
        // Closing an upgraded connection alone cannot guarantee that the
        // kubelet ended the command. Delete only the fenced pod.
        await this.remove(identity);
        throw watchdog.aborted ? timedOut() : gone();
      }
      if (error instanceof HttpError) throw error;
      await this.checked(identity); // distinguish an eviction from a broken transport
      throw unavailable();
    } finally {
      out.dispose();
      err.dispose();
    }
  }

  private async file(sandbox: DockerSandbox, path: string, operation: 'read' | 'write',
    signal?: AbortSignal, stdin?: string) {
    validatePath(path);
    const result = await this.exec(sandbox, {
      argv: [NODE, '-e', FILE_HELPER, operation, path], cwd: '/workspace', timeout_s: 10, ...(stdin !== undefined && { stdin }),
    }, signal);
    if (result.exit_code !== 0) throw new HttpError(422, 'sandbox.invalid_file', 'file is unavailable in the workspace');
    return { content: result.stdout, redacted: result.stdout_redacted === true };
  }

  read(sandbox: DockerSandbox, path: string, signal?: AbortSignal) {
    return this.file(sandbox, path, 'read', signal);
  }

  async write(sandbox: DockerSandbox, path: string, content: string, signal?: AbortSignal): Promise<void> {
    await this.file(sandbox, path, 'write', signal, content);
  }
}
