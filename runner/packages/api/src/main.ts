import http from 'node:http';
import { CapabilityVerifier } from '@runbook/runner-shared/capability';
import { loadConfig, loadInstallId } from '@runbook/runner-shared/config';
import { openPool } from '@runbook/runner-shared/db';
import { createJsonHandler } from '@runbook/runner-shared/http';
import { createLogger } from '@runbook/runner-shared/log';
import { AssertionVerifier } from './auth.ts';
import { syncDirectory, syncGroupProjects } from './directory-sync.ts';
import { deliverBudgetAlerts } from './ledger/alerts.ts';
import { stepCostRoute } from './ledger/step-cost.ts';
import { ledgerRoutes } from './ledger/routes.ts';
import { checkCoreContract } from './registration.ts';
import { sandboxMeterRoute } from './resources/activity.ts';
import { gatewayResources } from './resources/gateway.ts';
import { publicResources } from './resources/public.ts';
import { loadUsageMetrics } from './resources/usage.ts';
import { workerNotifier } from './scheduler/notify.ts';
import { coreWebhook } from './scheduler/webhook.ts';
import { scheduler } from './scheduler/service.ts';
import { createApiServer } from './server.ts';

export async function main(): Promise<void> {
  const config = loadConfig('api');
  const installId = loadInstallId();
  const coreUrl = config.coreUrl!;
  const token = config.coreToken!;
  const orgId = config.orgId!;
  const directory = { coreUrl, installId, token, orgId };
  await checkCoreContract(coreUrl);
  const db = openPool('api', config.databaseUrl!);
  await syncGroupProjects(db, directory);
  const ledger = openPool('scheduler', config.schedulerDatabaseUrl!);
  const log = createLogger('api');
  const verifier = new CapabilityVerifier({ coreUrl, runnerIds: config.runnerIds! });
  let alertDelivery: Promise<void> | undefined;
  const flushAlerts = () => {
    alertDelivery ??= deliverBudgetAlerts(ledger, { coreUrl, installId, token })
      .catch(() => log.error('budget event delivery failed'))
      .finally(() => { alertDelivery = undefined; });
  };
  const schedule = scheduler({ db: ledger, coreUrl, installId, token, orgId, log, verifier,
    notify: workerNotifier(ledger, log), wakeAlerts: flushAlerts });
  const webhook = coreWebhook(ledger, { secret: config.coreWebhookSecret, orgId, wake: schedule.wake });
  const internal = createJsonHandler({
    log, inboundTokens: config.inboundTokens, workerTokens: config.workerTokens,
    routes: [
      ...schedule.routes,
      ...gatewayResources(db, verifier, orgId),
      sandboxMeterRoute(ledger),
      ...ledgerRoutes(ledger, verifier, orgId, flushAlerts),
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
  const server = createApiServer(assertion, publicResources(db, config, sync, directory),
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
  const timer = setInterval(() => {
    sweep();
    flushAlerts();
    void db.query("DELETE FROM assertion_replays WHERE expires_at < now() - interval '1 minute'")
      .catch(() => log.error('assertion replay cleanup failed'));
    void sync();
  }, 60_000).unref();
  sweep();
  flushAlerts();
  void sync();
  const close = () => {
    clearInterval(timer);
    function closeServer(listener: http.Server) {
      return new Promise<void>((resolve) => listener.close(() => resolve()));
    }
    void Promise.allSettled([closeServer(server), closeServer(internalServer)])
      .then(() => Promise.allSettled([db.end(), ledger.end()]));

  };
  process.once('SIGTERM', close);
  process.once('SIGINT', close);
}
