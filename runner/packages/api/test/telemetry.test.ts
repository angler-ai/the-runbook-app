import { createServer, type Server } from 'node:http';
import { context, trace } from '@opentelemetry/api';
import { AsyncLocalStorageContextManager } from '@opentelemetry/context-async-hooks';
import { InMemorySpanExporter, SimpleSpanProcessor, TracerProvider } from '@opentelemetry/sdk-trace';
import { afterAll, beforeAll, expect, it, vi } from 'vitest';
import { coreJson } from '@runbook/runner-shared/core-client';
import { createJsonServer } from '@runbook/runner-shared/http';
import { captureLogs, createLogger, initLogRedaction } from '@runbook/runner-shared/log';
import {
  fetchWithTrace, identifier, inSpan, samplingRatio, setLabels, tracedServer, traceSampler,
} from '@runbook/runner-shared/telemetry';
import { createApiServer } from '../src/server.ts';
import { runClaim } from '../../worker/src/claim.ts';

const traceId = '1234567890abcdef1234567890abcdef';
const originSpan = 'aabbccddeeff0011';
const token = 'sk-ant-synthetic-credential-observability-marker';
const secret = 'raw-observability-test-secret';
const exporter = new InMemorySpanExporter();
const provider = new TracerProvider({
  sampler: traceSampler(0),
  spanProcessors: [new SimpleSpanProcessor({ exporter })],
});
const servers: Server[] = [];

beforeAll(async () => {
  context.setGlobalContextManager(new AsyncLocalStorageContextManager().enable());
  trace.setGlobalTracerProvider(provider);
  vi.stubEnv('GOOGLE_CLOUD_PROJECT', 'runbook-fixture');
  vi.stubEnv('RUNNER_TOKEN_OBSERVABILITY_TEST', secret);
  await initLogRedaction();
});

afterAll(async () => {
  await Promise.all(servers.map((server) => new Promise<void>((resolve) => server.close(() => resolve()))));
  await provider.shutdown();
  vi.unstubAllEnvs();
  trace.disable();
  context.disable();
});

async function start(server: Server) {
  servers.push(server);
  await new Promise<void>((resolve) => { server.listen(0, '127.0.0.1', resolve); });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('server did not start');
  return `http://127.0.0.1:${address.port}`;
}

it('defaults to 10% and validates the root ratio', () => {
  expect(['0', '1', '0.3', '', '2', '-1', 'NaN'].map(samplingRatio)).toEqual([0, 1, 0.3, 0.1, 0.1, 0.1, 0.1]);
  const always = new TracerProvider({ sampler: traceSampler(1) });
  expect(always.getTracer('test').startSpan('root').isRecording()).toBe(true);
});

it('follows the sampled parent across a core proxy, runner API, three runner steps and core reports', async () => {
  const logs = captureLogs();
  const workerLog = createLogger('worker', { org_id: 'org_fixture', project_id: 'prj_fixture', run_id: 'exe_fixture' });
  const vaultLog = createLogger('vault', { org_id: 'org_fixture' });
  const claim = { id: 'clm_fixture', generation: 1 };
  const steps = ['sr_fixture_1', 'sr_fixture_2', 'sr_fixture_3'];
  const received: string[] = [];
  try {
    const vault = await start(createJsonServer({
      log: vaultLog,
      inboundTokens: { worker: token },
      routes: [{
        method: 'POST', path: '/internal/release/{step_run_id}', callers: ['worker'],
        handle: ({ params }) => inSpan('credential.release', async () => {
          setLabels({ project: 'prj_fixture', run: 'exe_fixture', step_run: params.step_run_id! });
          vaultLog.info('released', { step_run_id: params.step_run_id, credential_id: 'cred_fixture', token, prompt: secret });
          return { body: { release_id: 'release_fixture' } };
        }),
      }],
    }));
    const core = await start(createServer(tracedServer((req, res) => {
      received.push(req.url ?? '');
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(req.url?.endsWith('/claim') ? claim : {}));
    })));
    const api = await start(createApiServer({
      verifyAssertion: async () => ({
        iss: core, aud: 'runner:fixture', jti: 'assertion', iat: 1, exp: 2,
        org_id: 'org_fixture', user_id: 'user_fixture', principal: 'human', org_role: 'owner',
        project_roles: {}, scopes: ['runner:read'], htm: 'POST', htu: '/runner/v1/activity', bh: 'fixture',
      }),
    }, async () => {
      const finished = await Promise.all(steps.map((step) => inSpan('runner.step', async () => {
        const coreOptions = { baseUrl: core, token, idempotencyKey: `key-${step}` };
        const path = `/v1/step-runs/${step}`;
        const result = await runClaim({ step_run_id: step, slot_id: step }, {
          claim: () => coreJson(coreOptions, 'POST', path + '/claim', { input: secret }),
          heartbeat: async () => undefined,
          capability: async () => ({ token, expiresAt: Date.now() + 60_000 }),
          refresh: async () => undefined,
          renewSandbox: async () => undefined,
          destroySandbox: async () => undefined,
          release: async () => undefined,
          abandon: async () => undefined,
          report: async () => coreJson(coreOptions, 'POST', path + '/report', { output: secret }),
          execute: async () => {
            const response = await fetchWithTrace(vault + '/internal/release/' + step, {
              method: 'POST', headers: { authorization: 'Bearer ' + token, 'content-type': 'application/json' },
              body: JSON.stringify({ secret }),
            });
            if (!response.ok) throw new Error('fixture release failed');
            await response.json();
            return { status: 'succeeded', outputs: {}, evidence: [], logs: [] };
          },
        }, new AbortController().signal);
        workerLog.info('runner step finished', { step_run_id: step });
        return result;
      })));
      return { body: { finished } };
    }));
    const proxy = await start(createServer(tracedServer((_, res) => {
      void (async () => {
        const response = await fetchWithTrace(api + '/runner/v1/activity', {
          method: 'POST', headers: { 'runbook-assertion': token },
        });
        res.writeHead(response.status).end(await response.text());
      })();
    })));
    const response = await fetch(proxy + '/v1/runners/install/proxy/runner/v1/activity', {
      headers: { traceparent: `00-${traceId}-${originSpan}-01`, baggage: `secret=${secret}` },
    });
    expect(await response.json()).toEqual({ finished: [true, true, true] });
    const debugLog = createLogger('worker');
    debugLog.debug('debug without opt-in');
    await fetch(vault + '/health/ready').then((res) => expect(res.status).toBe(404));
    expect(JSON.parse(logs.lines.pop()!)).toMatchObject({ status: 404, severity: 'WARNING' });
    expect(logs.lines).toHaveLength(6);
    expect(received).toHaveLength(6);
    const spans = exporter.getFinishedSpans();
    expect(spans).toHaveLength(27);
    expect(spans.every((span) => span.spanContext().traceId === traceId)).toBe(true);
    const spanIds = new Set(spans.map((span) => span.spanContext().spanId));
    expect(spans.every((span) => span.parentSpanContext &&
      (span.parentSpanContext.spanId === originSpan || spanIds.has(span.parentSpanContext.spanId)))).toBe(true);
    expect(logs.lines.every((line) => {
      const record = JSON.parse(line);
      return record['logging.googleapis.com/trace_sampled'] === true &&
        record['logging.googleapis.com/trace'] === `projects/runbook-fixture/traces/${traceId}`;
    })).toBe(true);
    expect(JSON.parse(logs.lines[0]!)['logging.googleapis.com/labels']).toMatchObject({
      org: 'org_fixture', project: 'prj_fixture', run: 'exe_fixture', runner_process: 'vault',
    });
    expect(logs.text() + JSON.stringify(spans.map((span) => span.attributes))).not.toContain(token);
    expect(logs.text() + JSON.stringify(spans.map((span) => span.attributes))).not.toContain(secret);
    expect(identifier(token)).toBeUndefined();
    if (process.env.RUNBOOK_TELEMETRY_MEASURE === '1') {
      const bytes = Buffer.byteLength(logs.lines.join('\n') + '\n');
      process.stdout.write(JSON.stringify({ runs: 1, steps: steps.length, lines: logs.lines.length, bytes, sampledSpans: spans.length }) + '\n');
    }
  } finally {
    logs.restore();
  }
});

it('drops a fresh root at ratio zero and preserves an unsampled parent', async () => {
  exporter.reset();
  const log = createLogger('api');
  const captured = captureLogs();
  const url = await start(createServer(tracedServer((_req, res) => {
    log.info('fixture');
    res.end('ok');
  })));
  try {
    await fetch(url).then((res) => res.text());
    await fetch(url, { headers: { traceparent: `00-${traceId}-${originSpan}-00` } }).then((res) => res.text());
    expect(exporter.getFinishedSpans()).toEqual([]);
    expect(JSON.parse(captured.lines[1]!)['logging.googleapis.com/trace_sampled']).toBe(false);
  } finally {
    captured.restore();
  }
});

it('logs capability auth failures at info and skips polling spans even with a sampled header', async () => {
  exporter.reset();
  const logs = captureLogs();
  const url = await start(createJsonServer({
    log: createLogger('vault'),
    inboundTokens: { worker: 'allowed-fixture', api: 'wrong-caller-fixture' },
    routes: [
      { method: 'POST', path: '/internal/capability', callers: ['worker'], handle: async () => ({ status: 204 }) },
      { method: 'POST', path: '/internal/slots/{slot_id}/heartbeat', callers: ['worker'], handle: async () => ({ status: 204 }) },
      ...['admission', 'wake'].map((name) => ({
        method: 'POST', path: '/internal/' + name, callers: ['worker' as const], handle: async () => ({ status: 204 }),
      })),
    ],
  }));
  vi.stubEnv('RUNBOOK_LOG_LEVEL', 'info');
  try {
    const call = (path: string, token: string) => fetch(url + path + '?fixture-query', {
      method: 'POST', headers: { authorization: 'Bearer ' + token, traceparent: `00-${traceId}-${originSpan}-01` },
    });
    expect((await call('/internal/capability', 'invalid-fixture')).status).toBe(401);
    expect((await call('/internal/capability', 'wrong-caller-fixture')).status).toBe(403);
    for (const path of ['capability', 'slots/fixture-slot/heartbeat', 'admission', 'wake']) {
      expect((await call('/internal/' + path, 'allowed-fixture')).status).toBe(204);
    }
    expect(logs.lines.map((line) => JSON.parse(line))).toMatchObject([401, 403].map((status) => ({
      severity: 'WARNING', method: 'POST', path: '/internal/capability', status,
    })));
    expect(logs.text()).not.toMatch(/fixture-query|invalid-fixture|wrong-caller-fixture/);
    expect(exporter.getFinishedSpans()).toEqual([]);
  } finally {
    logs.restore();
  }
});

it('reports suppressed warnings when the window closes and keeps distinct IDs visible', () => {
  vi.useFakeTimers();
  const logs = captureLogs();
  try {
    const log = createLogger('vault');
    const fields = { step_run_id: 'step-a', credential_id: 'credential-a' };
    log.warn('fixture denied', fields);
    log.warn('fixture denied', fields);
    log.warn('fixture denied', { ...fields, credential_id: 'credential-b' });
    log.warn('fixture denied', { ...fields, step_run_id: 'step-b' });
    expect(logs.lines).toHaveLength(3);
    vi.advanceTimersByTime(60_000);
    expect(logs.lines).toHaveLength(4);
    expect(JSON.parse(logs.lines[3]!)).toMatchObject({ ...fields, suppressed: 1 });
  } finally {
    logs.restore();
    vi.useRealTimers();
  }
});

it('redacts individual worker tokens and excludes email-shaped identifiers', async () => {
  const fixtures = {
    RUNNER_WORKER_API_TOKENS: 'sample-worker-api-value',
    RUNNER_WORKER_VAULT_TOKENS: 'sample-worker-vault-value',
    RUNNER_WORKER_GATEWAY_TOKENS: 'sample-worker-gateway-value',
    RUNNER_WORKER_NOTIFIERS: 'sample-worker-notifier-value',
  };
  for (const [key, token] of Object.entries(fixtures)) {
    vi.stubEnv(key, JSON.stringify({ worker: key.endsWith('NOTIFIERS') ? { url: 'https://worker.example', token } : token }));
  }
  await initLogRedaction();
  const logs = captureLogs();
  try {
    for (const token of Object.values(fixtures)) {
      createLogger('api').info('fixture', { reason: token });
      expect(logs.text()).not.toContain(token);
    }
    expect(identifier('somebody@example.test')).toBeUndefined();
    expect(identifier('provider/model:variant')).toBe('provider/model:variant');
  } finally {
    logs.restore();
  }
});
