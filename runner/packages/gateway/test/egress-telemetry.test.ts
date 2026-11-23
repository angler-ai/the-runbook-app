import { once } from 'node:events';
import http from 'node:http';
import https from 'node:https';
import { context, trace } from '@opentelemetry/api';
import { AsyncLocalStorageContextManager } from '@opentelemetry/context-async-hooks';
import { InMemorySpanExporter, SimpleSpanProcessor, TracerProvider } from '@opentelemetry/sdk-trace';
import { ProxyAgent, fetch as proxyFetch } from 'undici';
import { afterAll, beforeAll, expect, it, vi } from 'vitest';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import { inSpan, traceSampler } from '@runbook/runner-shared/telemetry';
import { MaterialStore } from '../src/material.ts';
import { pinnedFetch } from '../src/egress/fetch.ts';
import { sandboxListener } from '../src/egress/listener.ts';
import * as rules from '../src/egress/rules.ts';

const exporter = new InMemorySpanExporter();
const provider = new TracerProvider({ sampler: traceSampler(1), spanProcessors: [new SimpleSpanProcessor({ exporter })] });
const host = 'api.example';
const callerHeaders = {
  traceparent: '00-1234567890abcdef1234567890abcdef-aabbccddeeff0011-01',
  tracestate: 'fixture=redacted',
  baggage: 'fixture=redacted',
};

beforeAll(() => {
  context.setGlobalContextManager(new AsyncLocalStorageContextManager().enable());
  trace.setGlobalTracerProvider(provider);
});

afterAll(async () => {
  await provider.shutdown();
  trace.disable();
  context.disable();
});

async function withEcho(fn: (port: number) => Promise<void>) {
  const upstream = http.createServer((request, response) => response.end(JSON.stringify(request.headers)));
  upstream.listen(0, '127.0.0.1');
  await once(upstream, 'listening');
  const address = upstream.address();
  if (!address || typeof address === 'string') throw new Error('missing fixture address');
  const pin = vi.spyOn(rules, 'pinnedAddress').mockResolvedValue({ address: '127.0.0.1', family: 4 });
  try {
    await fn(address.port);
  } finally {
    pin.mockRestore();
    await new Promise<void>((resolve) => upstream.close(() => resolve()));
  }
}

it('keeps the client span without propagating context through the MCP, model and credential-test transport', async () => {
  exporter.reset();
  await withEcho(async (port) => {
    const input = new Request(`http://${host}:${port}/`, { headers: callerHeaders });
    const response = await inSpan('fixture.upstream', () => pinnedFetch(input));
    const echoed = await response.json() as Record<string, unknown>;
    for (const key of Object.keys(callerHeaders)) expect(echoed[key]).toBeUndefined();
  });
  expect(exporter.getFinishedSpans().map((span) => span.name)).toContain('http.client');
});

it('strips trace headers on sandbox egress and records up to 100 requests and clients per listener', async () => {
  exporter.reset();
  await withEcho(async (port) => {
    const send = vi.spyOn(https, 'request').mockImplementation(((options: http.RequestOptions, callback: (res: http.IncomingMessage) => void) =>
      http.request({ ...options, hostname: '127.0.0.1', port }, callback)) as typeof https.request);
    const store = new MaterialStore();
    const exp = Math.floor(Date.now() / 1000) + 60;
    const claims = { kind: 'capability', sub: 'step', claim_id: 'claim', generation: 1, runner_id: 'runner', exp } as GrantClaims;
    const layer = { [host]: 'read' as const };
    const layers = [layer, layer, layer];
    store.put({
      release_id: 'release', credential_id: 'fixture', step_run_id: claims.sub, claim_id: claims.claim_id,
      generation: claims.generation, runner_id: claims.runner_id, expires_at: new Date(exp * 1000).toISOString(), delivery: 'proxy',
      binding: { hosts: [host], header_name: 'x-fixture-key', header_format: '{}' },
      material: { fields: { key: 'fixture-value' } },
    });
    const listener = await inSpan('fixture.setup', () => sandboxListener({
      claims, store, layers, listenHost: '127.0.0.1', publicHost: '127.0.0.1',
      policy: async () => ({ layers, credentialLabels: { fixture: 'read' }, destructive: false }),
    }));
    const dispatcher = new ProxyAgent({
      uri: listener.proxy_url, token: 'Basic ' + Buffer.from(`runbook:${listener.listener_secret}`).toString('base64'),
      requestTls: { ca: listener.ca_pem },
    });
    try {
      for (let i = 0; i < 102; i++) {
        const response = await proxyFetch(`https://${host}/`, { dispatcher, headers: callerHeaders });
        expect(response.status).toBe(200);
        const echoed = await response.json() as Record<string, unknown>;
        for (const key of Object.keys(callerHeaders)) expect(echoed[key]).toBeUndefined();
      }
    } finally {
      await dispatcher.destroy();
      listener.close();
      store.clear();
      send.mockRestore();
    }
  });
  const spans = exporter.getFinishedSpans();
  expect(spans.filter((span) => span.name === 'egress.request')).toHaveLength(100);
  expect(spans.filter((span) => span.name === 'egress.http.client')).toHaveLength(100);
  const step = spans.find((span) => span.name === 'egress.step')!;
  expect(step.attributes['egress.requests_suppressed']).toBe(2);
  expect(spans.filter((span) => span.name === 'egress.request').every((span) =>
    span.parentSpanContext?.spanId === step.spanContext().spanId)).toBe(true);
});
