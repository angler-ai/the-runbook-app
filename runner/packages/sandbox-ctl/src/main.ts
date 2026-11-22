import Docker from 'dockerode';
import { CapabilityVerifier } from '@runbook/runner-shared/capability';
import { loadConfig, required } from '@runbook/runner-shared/config';
import { createJsonServer } from '@runbook/runner-shared/http';
import { createLogger } from '@runbook/runner-shared/log';
import { DockerSandboxBackend } from './docker.ts';
import { computeClient } from './meter.ts';
import { liveGrant, startReaper } from './reaper.ts';
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
  const gateway = required(process.env.RUNNER_GATEWAY_CONTAINER, 'RUNNER_GATEWAY_CONTAINER');
  const docker = new Docker({ socketPath: config.dockerSocket! });
  await docker.ping().catch(() => { throw new Error('Docker is unreachable'); });
  await requireDockerIsolation(docker);
  await docker.getImage(image).inspect().catch(() => { throw new Error('sandbox image is unavailable'); });
  const backend = new DockerSandboxBackend(docker, image, gateway, process.env.RUNNER_SANDBOX_SUBNET_POOL);
  const verifier = new CapabilityVerifier({ coreUrl: config.coreUrl!, runnerIds: config.runnerIds! });
  const log = createLogger('sandbox-ctl');
  const compute = computeClient(config);
  const cleaned = new Set(await backend.removeOrphans());
  async function flushSlots() {
    const finished = await Promise.allSettled([...cleaned].map(async (id) => {
      await compute.releaseSlot(id);
      cleaned.delete(id);
    }));
    if (finished.some(({ status }) => status === 'rejected')) log.warn('sandbox slot release incomplete');
  }
  await flushSlots();
  const service = sandboxService(backend, config, verifier, (id) => { cleaned.add(id); });
  const server = createJsonServer({ routes: service.routes, inboundTokens: config.inboundTokens, log });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(config.listen.port, config.listen.host, resolve);
  });
  const slotRetry = setInterval(() => { void flushSlots(); }, 15_000).unref();
  const stopReaper = startReaper(service.live, liveGrant(config, verifier), service.destroy,
    () => log.error('sandbox teardown incomplete'));
  const stop = () => {
    stopReaper();
    clearInterval(slotRetry);
    server.close(() => { void Promise.allSettled([...service.live.values()].map(service.destroy)).then(flushSlots); });
  };
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
}
