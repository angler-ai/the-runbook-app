import http from 'node:http';
import { CapabilityVerifier } from '@runbook/runner-shared/capability';
import { loadConfig, loadInstallId } from '@runbook/runner-shared/config';
import { openPool } from '@runbook/runner-shared/db';
import { createJsonHandler } from '@runbook/runner-shared/http';
import { createLogger, errorFields } from '@runbook/runner-shared/log';
import { AssertionVerifier } from './auth.ts';
import { syncDirectory, syncGroupProjects } from './directory-sync.ts';
import { startInstallationHeartbeat } from './installation-heartbeat.ts';
import { deliverBudgetAlerts } from './ledger/alerts.ts';
import { stepCostRoute } from './ledger/step-cost.ts';
import { ledgerRoutes } from './ledger/routes.ts';
import { checkCoreContract } from './registration.ts';
import { sandboxMeterRoute } from './resources/activity.ts';
import { installationImage, seedEnvironment } from './resources/environments.ts';
import { gatewayResources } from './resources/gateway.ts';
import { publicResources } from './resources/public.ts';
import { loadUsageMetrics } from './resources/usage.ts';
import { workerNotifier } from './scheduler/notify.ts';
import { coreWebhook } from './scheduler/webhook.ts';
import { scheduler } from './scheduler/service.ts';
import { retentionConfig } from './retention/config.ts';
import { runRetention } from './retention/job.ts';
import { archiveStore } from './retention/storage.ts';
import { createApiServer } from './server.ts';

export async function main(): Promise<void> {
  const retention = retentionConfig();
  const store = archiveStore(retention.location);
  const config = loadConfig('api');
  const installId = loadInstallId();
  const coreUrl = config.coreUrl!;
  const token = config.coreToken!;
  const orgId = config.orgId!;
  const directory = { coreUrl, installId, token, orgId };
  await checkCoreContract(coreUrl);
  const db = openPool('api', config.databaseUrl!);
  const archiveDb = retention.liveDays === null ? undefined : openPool('api', config.databaseUrl!, { max: 1 });
  const image = installationImage();
  if (image) await seedEnvironment(db, orgId, image);
  await syncGroupProjects(db, directory);
  const ledger = openPool('scheduler', config.schedulerDatabaseUrl!);
  const workerUrl = process.env.RUNNER_GKE_WORKER_URL;
  const capacity = Number(process.env.RUNNER_INSTALL_WORKER_CAPACITY);
  const runnerId = config.runnerIds?.[0];
  if (workerUrl && (!runnerId || config.runnerIds?.length !== 1 || !Number.isSafeInteger(capacity) || capacity < 1)) {
    throw new Error('invalid installed runner heartbeat configuration');
  }
  const log = createLogger('api');
  const verifier = new CapabilityVerifier({ coreUrl, runnerIds: config.runnerIds! });
  let alertDelivery: Promise<void> | undefined;
  const flushAlerts = () => {
    alertDelivery ??= deliverBudgetAlerts(ledger, { coreUrl, installId, token })
      .catch(() => log.error('budget event delivery failed'))
      .finally(() => { alertDelivery = undefined; });
  };
  const schedule = scheduler({ db: ledger, coreUrl, installId, token, orgId, log, verifier,
    notify: workerNotifier(ledger, log), wakeAlerts: flushAlerts, requireDefaultEnvironment: true });
  const webhook = coreWebhook(ledger, { secret: config.coreWebhookSecret, orgId, wake: schedule.wake });
  const internal = createJsonHandler({
    log, inboundTokens: config.inboundTokens, workerTokens: config.workerTokens,
    routes: [
      ...schedule.routes,
      ...gatewayResources(db, verifier, orgId),
      sandboxMeterRoute(ledger),
      ...ledgerRoutes(ledger, verifier, orgId, flushAlerts, true),
      stepCostRoute(ledger, verifier, flushAlerts),
    ],
  });
  const assertion = new AssertionVerifier({ db, coreUrl, installId, orgId });
  let previousSync = Promise.resolve();
  const sync = () => {
    previousSync = previousSync.then(() => syncDirectory(db, directory))
      .catch(() => log.error('directory sync failed'));
    return previousSync;
  };
  const server = createApiServer(assertion, publicResources(db, config, sync, directory, { store, orgId, archiveDays: retention.archiveDays }),
    config.coreWebhookSecret ? webhook : undefined, async () =>
      (await schedule.metrics()) + (await loadUsageMetrics(db)), config.metricsToken);
  const internalServer = http.createServer((req, res) => {
    if (req.url === '/internal/webhooks/core') webhook(req, res);
    else internal(req, res);
  });
  function listen(listener: http.Server, address: ConfigAddress) {
    return new Promise<void>((resolve, reject) => {
      listener.once('error', reject);
      listener.listen(address.port, address.host, resolve);
    });
  }
  type ConfigAddress = typeof config.listen;
  await listen(internalServer, config.internalListen!);
  try {
    await listen(server, config.listen);
  } catch (error) {
    internalServer.close();
    throw error;
  }
  const sweep = () => { void schedule.sweep().catch(() => log.error('scheduler sweep failed')); };
  let archival: Promise<void> | undefined;
  const archive = () => {
    if (!archiveDb) return;
    archival ??= runRetention(archiveDb, store, orgId, retention)
      .then((steps) => { if (steps) log.info('runner detail archived', { count: steps }); })
      .catch((error) => log.error('runner retention failed', errorFields(error)))
      .finally(() => { archival = undefined; });
  };
  const timer = setInterval(() => {
    archive();
    sweep();
    flushAlerts();
    void db.query("DELETE FROM assertion_replays WHERE expires_at < now() - interval '1 minute'")
      .catch(() => log.error('assertion replay cleanup failed'));
    void sync();
  }, 60_000).unref();
  sweep();
  archive();
  flushAlerts();
  void sync();
  const stopHeartbeat = workerUrl && runnerId ? startInstallationHeartbeat({
    coreUrl, installId, token, workerUrl, runnerId, capacity, pool: ledger,
  }, () => log.error('runner installation heartbeat failed')) : undefined;
  const close = () => {
    clearInterval(timer);
    stopHeartbeat?.();
    function closeServer(listener: http.Server) {
      return new Promise<void>((resolve) => listener.close(() => resolve()));
    }
    void Promise.allSettled([closeServer(server), closeServer(internalServer), archival])
      .then(() => Promise.allSettled([db.end(), ledger.end(), archiveDb?.end()]));

  };
  process.once('SIGTERM', close);
  process.once('SIGINT', close);
}
