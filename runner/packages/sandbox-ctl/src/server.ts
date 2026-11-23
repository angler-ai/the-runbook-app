import { fetchWithTrace, inSpan, setLabels } from '@runbook/runner-shared/telemetry';
import { randomUUID } from 'node:crypto';
import { CREDENTIAL_PLACEHOLDER_PREFIX } from '@runbook/runner-shared/credential-source';
import { sandboxIdForClaim, type CapabilityVerifier } from '@runbook/runner-shared/capability';
import type { Config } from '@runbook/runner-shared/config';
import type { ExecRequest, SandboxListenerResponse, SandboxSize } from '@runbook/runner-shared/contracts/internal';
import { HttpError, type Route, type RouteContext } from '@runbook/runner-shared/http';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import { type DockerSandbox, DockerSandboxBackend, type SandboxNetwork } from './docker.ts';
import { KubernetesSandboxBackend } from './kubernetes.ts';
import { enforceKubernetesProbe } from './kubernetes-probe.ts';
import { isIPv4 } from 'node:net';
import { computeClient, type ComputeHold } from './meter.ts';
import { enforceProbe } from './probe.ts';
import { SIZES } from './template.ts';
import { refreshedGrant } from './reaper.ts';

const unavailable = () => new HttpError(503, 'sandbox.unavailable', 'sandbox operation unavailable');
const gone = () => new HttpError(410, 'sandbox.gone', 'sandbox container is gone');
const missing = () => new HttpError(404, 'sandbox.not_found', 'sandbox unavailable');

export interface RunningSandbox {
  sandbox: DockerSandbox;
  capability: string;
  claims: GrantClaims;
  meter: ComputeHold;
  started: number;
  fenced?: boolean;
  stopped?: number;
  stoppedAt?: number;
  timer: NodeJS.Timeout;
  setup?: { script: string; timeout: number; state: 'pending' | 'running' | 'done' | 'failed' };
}

export function sandboxService(backend: DockerSandboxBackend | KubernetesSandboxBackend, config: Config,
  verifier: Pick<CapabilityVerifier, 'verifyCapability'>, onDestroyed: (id: string) => void = () => {},
  options: { kubernetes?: boolean; environments?: boolean } = {}) {
  const compute = computeClient(config);
  const live = new Map<string, RunningSandbox>();
  const creating = new Map<string, AbortController>();
  const deleting = new Map<string, Promise<void>>();

  async function gateway(method: 'POST' | 'DELETE', path: string, body: object) {
    try {
      return await fetchWithTrace(`${config.urls.gateway}/internal/sandboxes${path}`, {
        method, headers: { 'content-type': 'application/json', authorization: `Bearer ${config.outboundTokens.gateway}` },
        body: JSON.stringify(body), redirect: 'error', signal: AbortSignal.timeout(4000),
      });
    } catch {
      throw unavailable();
    }
  }

  async function deleteListener(id: string, capability: string) {
    const reply = await gateway('DELETE', `/${id}`, { capability });
    if (!reply.ok && reply.status !== 404 && reply.status !== 401) throw unavailable();
  }

  function destroy(entry: RunningSandbox): Promise<void> {
    const { id, network, resources } = entry.sandbox;
    const existing = deleting.get(id);
    if (existing) return existing;
    entry.fenced = true;
    const task = (async () => {
      // Revoke the listener even if the pod API cannot delete the sandbox.
      const [removal, listener] = await Promise.allSettled([
        backend.destroy(id, network),
        deleteListener(id, entry.capability),
      ]);
      if (removal.status === 'rejected' && entry.stopped === undefined) throw removal.reason;
      clearTimeout(entry.timer);
      entry.stopped ??= Math.floor(performance.now());
      entry.stoppedAt ??= Date.now();
      await Promise.all([
        entry.meter.settle(entry.stopped - entry.started, resources),
        compute.sandbox(id, 'stopped', entry.stoppedAt),
      ]);
      if (removal.status === 'rejected') throw removal.reason;
      if (listener.status === 'rejected') throw listener.reason;
      live.delete(id);
      onDestroyed(id);
    })().finally(() => deleting.delete(id));
    deleting.set(id, task);
    return task;
  }

  async function verify(id: string | undefined, capability: unknown) {
    if (typeof capability !== 'string') throw new HttpError(401, 'capability.invalid', 'the step capability is invalid');
    const claims = await verifier.verifyCapability(capability);
    setLabels({ project: claims.project_id, step_run: claims.sub });
    if (id !== undefined && id !== sandboxIdForClaim(claims)) throw missing();
    return { capability, claims };
  }

  async function forRoute(ctx: RouteContext, forWork = false) {
    const { id } = ctx.params;
    await verify(id, ctx.request?.headers['x-runbook-capability']);
    if (creating.has(id ?? '')) throw new HttpError(409, 'sandbox.creating', 'sandbox creation in progress');
    const entry = live.get(id ?? '');
    if (!entry || deleting.has(id ?? '') || (forWork && entry.fenced)) throw forWork ? gone() : missing();
    return entry;
  }

  async function withSandbox<T>(ctx: RouteContext, operation: (sandbox: DockerSandbox, signal?: AbortSignal) => Promise<T>) {
    const entry = await forRoute(ctx, true);
    if (entry.setup && entry.setup.state !== 'done') throw new HttpError(409, 'sandbox.setup_required', 'sandbox setup is incomplete');
    try {
      return await operation(entry.sandbox, options.kubernetes ? ctx.request?.signal : undefined);
    } catch (error) {
      if (error instanceof HttpError && error.code === 'sandbox.gone') {
        entry.stopped ??= Math.floor(performance.now());
        entry.stoppedAt ??= Date.now();
        await destroy(entry).catch(() => undefined);
      }
      throw error;
    }
  }

  const callers = ['worker'] as const;
  const routes: Route[] = [{
    method: 'POST', path: '/internal/sandboxes', callers,
    handle: ({ body, request }) => inSpan('sandbox.create', async () => {
      const input = body as { capability?: unknown; size?: SandboxSize; image?: unknown } | undefined;
      let { claims, capability } = await verify(undefined, input?.capability);
      const environment = options.environments ? await compute.environment(capability) : undefined;
      const image = environment?.image ?? backend.image;
      if (!input?.size || !Object.hasOwn(SIZES, input.size) || input.image !== image ||
          (environment && input.size !== environment.size)) {
        throw new HttpError(422, 'sandbox.invalid_template', 'invalid sandbox template');
      }
      const id = sandboxIdForClaim(claims);
      if (live.has(id) || creating.has(id)) throw new HttpError(409, 'sandbox.exists', 'sandbox already exists');
      const creation = new AbortController();
      creating.set(id, creation);
      const signal = options.kubernetes && request?.signal
        ? AbortSignal.any([creation.signal, request.signal]) : creation.signal;
      let network: SandboxNetwork | undefined;
      let listenerRequested = false;
      let hold: ComputeHold | undefined;
      let started: number | undefined;
      let resources: DockerSandbox['resources'];
      let entry: RunningSandbox | undefined;
      try {
        if (environment?.custom && backend instanceof DockerSandboxBackend) await backend.requireLocalImage(image);
        network = await backend.allocateNetwork(id);
        signal.throwIfAborted();
        if (!options.kubernetes && !network.gatewayAddress) throw unavailable();
        const provision = async () => {
          listenerRequested = true;
          const reply = await gateway('POST', '', {
            sandbox_id: id, capability, listen_host: network!.gatewayAddress,
          });
          if (!reply.ok) throw unavailable();
          const listener = await reply.json() as SandboxListenerResponse;
          const proxy = new URL(listener.proxy_url);
          if (proxy.protocol !== 'http:' || (options.kubernetes
            ? !isIPv4(proxy.hostname) || Number(proxy.port) < 20000 || Number(proxy.port) > 29999
            : proxy.hostname !== network!.gatewayAddress) || proxy.username || proxy.password ||
            !proxy.port || !listener.listener_secret || !listener.ca_pem) throw unavailable();
          return listener;
        };
        const dockerListener = options.kubernetes ? undefined : await provision();
        const rates = await compute.rates(capability);
        // Autopilot can adjust requests. Reserve the maximum footprint
        // allowed by the sandbox admission policy before creating a pod.
        const estimate = options.kubernetes ? SIZES.large : undefined;
        const admission = await compute.reserve(capability, input.size, rates, estimate);
        if ('level' in admission) {
          await backend.destroy(id, network);
          if (listenerRequested) await deleteListener(id, capability);
          return { status: 402, body: admission };
        }
        hold = admission;
        signal.throwIfAborted();
        const sandbox = backend instanceof KubernetesSandboxBackend
          ? await backend.start(id, network, input.size, async () => {
            signal.throwIfAborted();
            if (claims.exp * 1000 - Date.now() < 15_000) {
              const fresh = await refreshedGrant(config, verifier, claims);
              if (!fresh) throw gone();
              ({ claims, capability } = fresh);
            }
            return provision();
          }, hold.timeoutMs, claims, signal, (admitted, at) => {
            resources = admitted;
            started = Math.floor(performance.now()) - Math.max(0, Date.now() - at);
            if (admitted.vcpu > SIZES.large.vcpu || admitted.gib > SIZES.large.gib) throw unavailable();
          }, image)
          : await backend.start(id, network, input.size, dockerListener!, hold.timeoutMs, image,
            { custom: environment?.custom, setup: Boolean(environment?.setup) });
        started ??= Math.floor(performance.now());
        const startedAt = sandbox.startedAt ?? Date.now();
        const timer = setTimeout(() => {
          const current = live.get(id);
          if (current) void destroy(current).catch(() => undefined);
        }, Math.max(0, hold.timeoutMs - (performance.now() - started))).unref();
        const running: RunningSandbox = { sandbox, capability, claims, meter: hold, started, timer };
        if (environment?.setup) running.setup = { script: environment.setup, timeout: environment.setup_timeout_s, state: 'pending' };
        entry = running;
        live.set(id, running);
        await compute.sandbox(id, 'started', startedAt);
        signal.throwIfAborted();
        if (options.kubernetes) {
          await enforceKubernetesProbe((command) => sandbox.customImage && backend instanceof KubernetesSandboxBackend
            ? backend.customProbe(sandbox, signal) : backend.exec(sandbox, command, signal),
            () => compute.alert(capability));
        } else {
          await enforceProbe((command) => sandbox.customImage && backend instanceof DockerSandboxBackend
            ? backend.customProbe(sandbox, network?.bridgeGateway) : backend.exec(sandbox, command),
            () => destroy(running), () => compute.alert(capability), network.bridgeGateway);
        }
        signal.throwIfAborted();
        if (!live.has(id)) throw missing();
        if (environment) sandbox.env = {
          ...environment.env,
          ...Object.fromEntries(environment.credential_names.map((name) => [name, `${CREDENTIAL_PLACEHOLDER_PREFIX}${randomUUID()}`])),
        };
        await compute.sandbox(id, 'ready');
        signal.throwIfAborted();
        if (!live.has(id)) throw gone();
        return { status: 201, body: { sandbox_id: id } };
      } catch (error) {
        if (entry && live.has(id)) await destroy(entry).catch(() => undefined);
        if (!entry) {
          if (network) await backend.destroy(id, network).catch(() => undefined);
          if (hold) await hold.settle(started === undefined ? 0 : Math.floor(performance.now()) - started, resources).catch(() => undefined);
          if (listenerRequested) await deleteListener(id, capability).catch(() => undefined);
        }
        throw error instanceof HttpError ? error : unavailable();
      } finally {
        creating.delete(id);
      }
    }),
  }, {
    method: 'POST', path: '/internal/sandboxes/{id}/renew', callers,
    async handle(ctx) {
      const entry = await forRoute(ctx, true);
      const { sandbox } = entry;
      const fresh = await verify(sandbox.id, ctx.request?.headers['x-runbook-capability']);
      const reply = await gateway('POST', `/${sandbox.id}/renew`, { capability: fresh.capability });
      if (reply.status === 404) {
        await destroy(entry).catch(() => undefined);
        throw gone();
      }
      if (!reply.ok) throw unavailable();
      if (live.get(sandbox.id) !== entry || entry.fenced || deleting.has(sandbox.id)) throw missing();
      Object.assign(entry, fresh);
      return { status: 204 };
    },
  }, {
    method: 'POST', path: '/internal/sandboxes/{id}/setup', callers,
    async handle(ctx) {
      const entry = await forRoute(ctx, true);
      const setup = entry.setup;
      if (!setup || setup.state !== 'pending') throw new HttpError(409, 'sandbox.setup_unavailable', 'sandbox setup unavailable');
      setup.state = 'running';
      try {
        const timeoutMs = Math.min(setup.timeout * 1000,
          Math.floor(entry.meter.timeoutMs - (performance.now() - entry.started)));
        if (timeoutMs <= 0) throw new HttpError(408, 'sandbox.exec_timeout', 'sandbox execution timed out');
        const result = await backend.exec(entry.sandbox, {
          argv: ['/bin/bash', '--noprofile', '--norc', '-s'], stdin: setup.script, cwd: '/workspace',
          timeout_s: timeoutMs / 1000,
          return_partial: true,
        }, ctx.request?.signal, true);
        setup.state = result.exit_code === 0 && !result.timed_out ? 'done' : 'failed';
        if (setup.state === 'failed') await destroy(entry).catch(() => undefined);
        return { body: result };
      } catch (error) {
        await destroy(entry).catch(() => undefined);
        throw error;
      } finally {
        if (setup.state === 'running') setup.state = 'failed';
      }
    },
  }, {
    method: 'POST', path: '/internal/sandboxes/{id}/exec', callers,
    async handle(ctx) {
      if (!ctx.body || typeof ctx.body !== 'object') throw new HttpError(422, 'sandbox.invalid_exec', 'invalid sandbox execution request');
      return { body: await withSandbox(ctx, (sandbox, signal) => backend.exec(sandbox, ctx.body as ExecRequest, signal)) };
    },
  }, {
    method: 'GET', path: '/internal/sandboxes/{id}/files/*', callers,
    async handle(ctx) {
      return { body: await withSandbox(ctx, (sandbox, signal) => backend.read(sandbox, ctx.params['*'] ?? '', signal)) };
    },
  }, {
    method: 'PUT', path: '/internal/sandboxes/{id}/files/*', callers,
    async handle(ctx) {
      const content = (ctx.body as { content?: unknown } | null)?.content;
      if (typeof content !== 'string') throw new HttpError(422, 'sandbox.invalid_file', 'invalid workspace content');
      await withSandbox(ctx, (sandbox, signal) => backend.write(sandbox, ctx.params['*'] ?? '', content, signal));
      return { status: 204 };
    },
  }, {
    method: 'DELETE', path: '/internal/sandboxes/{id}', callers,
    async handle(ctx) {
      const id = ctx.params.id ?? '';
      await verify(id, ctx.request?.headers['x-runbook-capability']);
      creating.get(id)?.abort();
      const entry = await forRoute(ctx);
      await inSpan('sandbox.destroy', () => destroy(entry));
      return { status: 204 };
    },
  }];
  return { routes, live, destroy, owns: (id: string, uid?: string) => {
    const entry = live.get(id);
    return entry ? uid === undefined || uid === entry.sandbox.containerId : creating.has(id);
  } };
}
