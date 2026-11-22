import { CapabilityVerifier } from '@runbook/runner-shared/capability';
import { loadConfig, required } from '@runbook/runner-shared/config';
import { loadRedaction } from '@runbook/runner-shared/redact';
import { createJsonServer } from '@runbook/runner-shared/http';
import { createLogger } from '@runbook/runner-shared/log';
import { admissionLoop } from './admission.ts';
import { runClaim } from './claim.ts';
import { claimExecutor } from './execute.ts';
import { workerClients } from './clients.ts';

export async function main(): Promise<void> {
  const config = loadConfig('worker');
  const capacity = Number(process.env.RUNNER_CAPACITY ?? 1);
  if (!Number.isSafeInteger(capacity) || capacity < 1 || capacity > 1000) throw new Error('invalid RUNNER_CAPACITY');
  const log = createLogger('worker', { runner_id: config.runnerId, group_id: config.groupId });
  const clients = workerClients(config);
  const verifier = new CapabilityVerifier({ coreUrl: config.coreUrl!, runnerIds: [config.runnerId!] });
  const image = required(process.env.RUNNER_SANDBOX_IMAGE, 'RUNNER_SANDBOX_IMAGE');
  const redaction = await loadRedaction();
  const installId = required(process.env.RUNNER_INSTALL_ID, 'RUNNER_INSTALL_ID');
  const worker = admissionLoop(capacity, clients.admit, (item, signal) => runClaim(item, {
    ...clients,
    onSlotReleaseFailure: () => log.warn('scheduler slot release incomplete'),
    execute: claimExecutor(config, clients, verifier, image, { redaction, installId }),
  }, signal), () => log.error('worker admission or teardown failed'));
  const server = createJsonServer({ log, decisionSupport: true, inboundTokens: config.inboundTokens, routes: [{
    method: 'POST', path: '/internal/wake', callers: ['api'],
    async handle() {
      worker.wake();
      return { status: 204 };
    },
  }] });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(config.listen.port, config.listen.host, resolve);
  });
  const close = () => {
    server.close();
    void worker.close();
  };
  process.once('SIGINT', close);
  process.once('SIGTERM', close);
}
