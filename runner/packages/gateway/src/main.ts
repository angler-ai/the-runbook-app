import { HttpError } from '@runbook/runner-shared/http';
import { loadConfig } from '@runbook/runner-shared/config';
import { createLogger } from '@runbook/runner-shared/log';
import { loadRedaction } from '@runbook/runner-shared/redact';
import { CapabilityVerifier } from './capability.ts';
import { testConnectionRoute } from './egress/test-connection.ts';
import { sandboxRoute } from './egress/route.ts';
import { stepRedactionRoute } from './redact-route.ts';
import { MaterialStore } from './material.ts';
import { discoveryRoute, mcpRoute } from './mcp/server.ts';
import { modelRoute } from './model/routes.ts';
import { createGatewayServer } from './server.ts';

export async function main(): Promise<void> {
  process.report.reportOnFatalError = false;
  process.report.reportOnSignal = false;
  process.report.reportOnUncaughtException = false;
  const config = loadConfig('gateway');
  const log = createLogger('gateway');
  const store = new MaterialStore();
  const verifier = new CapabilityVerifier({ coreUrl: config.coreUrl!, runnerIds: config.runnerIds! });
  const redaction = await loadRedaction();
  const sandboxes = sandboxRoute(config, verifier, store, process.env.GATEWAY_SANDBOX_HOST ?? 'gateway');
  const soleRunner = config.runnerIds?.length === 1 ? config.runnerIds[0] : undefined;
  const server = createGatewayServer({ decisionSupport: true, inboundTokens: config.inboundTokens, workerTokens: config.workerTokens, log }, store, [
    stepRedactionRoute(verifier, store, redaction, soleRunner, Date.now, sandboxes.hasSandbox),
    stepRedactionRoute(verifier, store, redaction, soleRunner, Date.now, sandboxes.hasSandbox, async (record, signal) => {
      const timeout = AbortSignal.timeout(10_000);
      const result = await fetch(`${config.urls.api}/internal/transcripts`, {
        method: 'POST', redirect: 'error', body: JSON.stringify(record),
        headers: { authorization: `Bearer ${config.outboundTokens.api}`, 'content-type': 'application/json' },
        signal: signal ? AbortSignal.any([timeout, signal]) : timeout,
      }).catch(() => undefined);
      if (!result?.ok) throw new HttpError(503, 'transcript.record_unavailable', 'transcript could not be recorded');
    }),
    modelRoute(config, verifier, store, redaction),
    modelRoute(config, verifier, store, redaction, true),
    mcpRoute(config, verifier, store, redaction),
    discoveryRoute(store, redaction),
    ...sandboxes.routes,
    testConnectionRoute(store),
  ]);
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(config.listen.port, config.listen.host, resolve);
  });
  const shutdown = () => { sandboxes.close(); server.close(); };
  process.once('SIGTERM', shutdown);
  process.once('SIGINT', shutdown);
}
