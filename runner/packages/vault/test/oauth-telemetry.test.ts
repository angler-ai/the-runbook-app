import { context, trace } from '@opentelemetry/api';
import { AsyncLocalStorageContextManager } from '@opentelemetry/context-async-hooks';
import { InMemorySpanExporter, SimpleSpanProcessor, TracerProvider } from '@opentelemetry/sdk-trace';
import { fetch as proxyFetch, Response } from 'undici';
import { expect, it, vi } from 'vitest';
import { traceSampler } from '@runbook/runner-shared/telemetry';
import { TunneledOAuthProvider } from '../src/oauth/provider.ts';

vi.mock(import('undici'), async (original) => ({
  ...await original(),
  fetch: vi.fn(),
}));

it('keeps the OAuth client span and strips trace headers from the provider request', async () => {
  const exporter = new InMemorySpanExporter();
  const provider = new TracerProvider({ sampler: traceSampler(1), spanProcessors: [new SimpleSpanProcessor({ exporter })] });
  context.setGlobalContextManager(new AsyncLocalStorageContextManager().enable());
  trace.setGlobalTracerProvider(provider);
  vi.mocked(proxyFetch).mockResolvedValue(new Response('{"fixture":true}'));
  try {
    const oauth = new TunneledOAuthProvider('http://127.0.0.1:1', 'fixture');
    const result = await oauth.request('https://provider.example/token', {
      method: 'POST', body: 'fixture', headers: { TraceState: 'fixture=upstream', baggage: 'fixture=upstream', accept: 'application/json' },
    });
    expect(result.body).toEqual({ fixture: true });
    const headers = new Headers(vi.mocked(proxyFetch).mock.lastCall?.[1]?.headers as Record<string, string>);
    expect(Object.fromEntries(headers)).toEqual({ accept: 'application/json' });
    expect(exporter.getFinishedSpans().map((span) => span.name)).toEqual(['oauth.http.client']);
  } finally {
    await provider.shutdown();
    trace.disable();
    context.disable();
  }
});
