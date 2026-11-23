import Docker from 'dockerode';
import { CapabilityVerifier } from '@runbook/runner-shared/capability';
import { loadConfig, required } from '@runbook/runner-shared/config';
import { createJsonServer } from '@runbook/runner-shared/http';
import { createLogger } from '@runbook/runner-shared/log';
import { DockerSandboxBackend } from './docker.ts';
import { KubernetesApi } from './kubernetes-api.ts';
import { KubernetesSandboxBackend } from './kubernetes.ts';
import { computeClient } from './meter.ts';
import { gatewayListenerAlive, liveGrant, startReaper } from './reaper.ts';
import { sandboxService } from './server.ts';

export async function requireDockerIsolation(docker: Pick<Docker, 'version' | 'info'>): Promise<void> {
  const [version, info] = await Promise.all([docker.version(), docker.info()]);
  const major = /^(\d+)\./.exec(version.Version)?.[1];
  if (major === undefined || Number(major) < 25) throw new Error('Docker Engine 25 or newer is required');
  const seccomp = (info.SecurityOptions as string[] | undefined)?.find((value) =>
    value.split(',').includes('name=seccomp'));
  if (!seccomp || seccomp.split(',').includes('profile=unconfined')) {
    throw new Error('Docker must have a confined seccomp profile enabled');
  }
}

export async function main(): Promise<void> {
  const config = loadConfig('sandbox-ctl');
  const image = required(process.env.RUNNER_SANDBOX_IMAGE, 'RUNNER_SANDBOX_IMAGE');
  const mode = process.env.RUNNER_SANDBOX_BACKEND ?? 'docker';
  if (mode !== 'docker' && mode !== 'kubernetes') throw new Error('invalid RUNNER_SANDBOX_BACKEND');
  const { backend, localDigest } = mode === 'kubernetes'
    ? { localDigest: undefined, backend: new KubernetesSandboxBackend(KubernetesApi.inCluster(process.env.RUNNER_SANDBOX_NAMESPACE ?? 'runbook-sandboxes'),
      image, undefined, {
        namePrefix: process.env.RUNNER_SANDBOX_NAME_PREFIX,
        serviceAccountName: process.env.RUNNER_SANDBOX_SERVICE_ACCOUNT,
        labels: sandboxLabels(process.env.RUNNER_SANDBOX_LABELS),
      }) }
    : await dockerBackend(image, config.dockerSocket!);
  const verifier = new CapabilityVerifier({ coreUrl: config.coreUrl!, runnerIds: config.runnerIds! });
  const log = createLogger('sandbox-ctl');
  const compute = computeClient(config);
  const seed = () => localDigest ? compute.seedEnvironment(localDigest) : Promise.resolve();
  await seed().catch(() => log.warn('default environment registration will retry'));
  const cleaned = new Set(await backend.removeOrphans());
  let slotFlush: Promise<void> | undefined;
  function flushSlots(): Promise<void> {
    slotFlush ??= flush().finally(() => { slotFlush = undefined; });
    return slotFlush;
  }
  async function flush() {
    const finished = await Promise.allSettled([...cleaned].map(async (id) => {
      await compute.releaseSlot(id);
      cleaned.delete(id);
    }));
    if (finished.some(({ status }) => status === 'rejected')) log.warn('sandbox slot release incomplete');
  }
  await flushSlots();
  const service = sandboxService(backend, config, verifier, (id) => { cleaned.add(id); },
    { kubernetes: mode === 'kubernetes', environments: true });
  let reaping = false;
  const orphanRetry = backend instanceof KubernetesSandboxBackend ? setInterval(() => {
    if (reaping) return;
    reaping = true;
    void backend.removeOrphans(service.owns)
      .then((ids) => ids.forEach((id) => cleaned.add(id)))
      .catch(() => log.error('sandbox orphan cleanup incomplete'))
      .finally(() => { reaping = false; });
  }, 15_000).unref() : undefined;
  const server = createJsonServer({ routes: service.routes, inboundTokens: config.inboundTokens, log });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(config.listen.port, config.listen.host, resolve);
  });
  const slotRetry = setInterval(() => { void flushSlots(); }, 15_000).unref();
  const seedRetry = localDigest && setInterval(() => { void seed().catch(() => undefined); }, 60_000).unref();
  const grantIsLive = liveGrant(config, verifier);
  const stopReaper = startReaper(service.live, async (entry) =>
    (backend instanceof KubernetesSandboxBackend ? await backend.isRunning(entry.sandbox) : true) &&
    await gatewayListenerAlive(config, entry).catch(() => true) && await grantIsLive(entry),
    service.destroy, () => log.error('sandbox teardown incomplete'));
  const stop = () => {
    stopReaper();
    clearInterval(slotRetry);
    if (seedRetry) clearInterval(seedRetry);
    clearInterval(orphanRetry);
    server.close(() => { void Promise.allSettled([...service.live.values()].map(service.destroy)).then(flushSlots); });
  };
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
}

function sandboxLabels(raw = '{}'): Record<string, string> {
  const labels: unknown = JSON.parse(raw);
  if (labels === null || typeof labels !== 'object' || Array.isArray(labels) ||
      Object.values(labels).some((value) => typeof value !== 'string')) {
    throw new Error('RUNNER_SANDBOX_LABELS must be a JSON object with string values');
  }
  return labels as Record<string, string>;
}

async function dockerBackend(image: string, socketPath: string) {
  const gateway = required(process.env.RUNNER_GATEWAY_CONTAINER, 'RUNNER_GATEWAY_CONTAINER');
  const docker = new Docker({ socketPath: socketPath });
  await docker.ping().catch(() => { throw new Error('Docker is unreachable'); });
  await requireDockerIsolation(docker);
  const inspected = await docker.getImage(image).inspect().catch(() => { throw new Error('sandbox image is unavailable'); });
  return { localDigest: inspected.Id, backend: new DockerSandboxBackend(docker, image, gateway, process.env.RUNNER_SANDBOX_SUBNET_POOL) };
}
