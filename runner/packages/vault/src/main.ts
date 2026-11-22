// The vault process (`runner vault`). Its own container and identity: the
// KEK (local key file or Cloud KMS through its attached service account),
// its rb_vault database login, its release-only core token, and the gateway
// it pushes material to. Inbound only from runner-api and runner-worker.
import { loadConfig, loadInstallId, required } from '@runbook/runner-shared/config';
import { openPool } from '@runbook/runner-shared/db';
import { createLogger } from '@runbook/runner-shared/log';
import { createKek } from './kek/index.ts';
import { createVaultServer } from './server.ts';
import { Vault } from './vault.ts';

export async function main(): Promise<void> {
  // No diagnostic reports: they would capture memory that can hold material.
  process.report.reportOnFatalError = false;
  process.report.reportOnSignal = false;
  process.report.reportOnUncaughtException = false;

  const config = loadConfig('vault');
  const log = createLogger('vault');
  const kek = await createKek(required(config.kek, 'RUNNER_KEK_PROVIDER'));
  const pool = openPool('vault', required(config.databaseUrl, 'RUNNER_DATABASE_URL'));
  const vault = await Vault.create({
    pool,
    kek,
    orgId: required(config.orgId, 'RUNNER_ORG_ID'),
    log,
    assertion: {
      coreUrl: required(config.coreUrl, 'CORE_URL'),
      installId: loadInstallId(),
    },
    core: { baseUrl: required(config.coreUrl, 'CORE_URL'), releaseToken: required(config.coreToken, 'CORE_RELEASE_TOKEN') },
    gateway: {
      baseUrl: required(config.urls.gateway, 'GATEWAY_URL'),
      token: required(config.outboundTokens.gateway, 'RUNNER_TOKEN_VAULT_GATEWAY'),
    },
  });
  const server = createVaultServer(vault, { inboundTokens: config.inboundTokens, workerTokens: config.workerTokens,
    soleRunner: config.runnerIds?.length === 1 ? config.runnerIds[0] : undefined });
  await new Promise<void>((resolve) => server.listen(config.listen.port, config.listen.host, resolve));
  log.info('vault listening', { host: config.listen.host, port: config.listen.port });

  const shutdown = () => {
    log.info('vault stopping');
    vault.stop();
    server.close(() => void pool.end().finally(() => process.exit(0)));
    setTimeout(() => process.exit(0), 8_000).unref();
  };
  process.once('SIGTERM', shutdown);
  process.once('SIGINT', shutdown);
  process.on('uncaughtException', (e) => {
    log.error('uncaught exception', { error: e.name });
    process.exit(1);
  });
  process.on('unhandledRejection', (e) => {
    log.error('unhandled rejection', { error: e instanceof Error ? e.name : 'error' });
    process.exit(1);
  });
}
