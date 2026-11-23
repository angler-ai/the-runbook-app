import type { IncomingHttpHeaders, RequestListener } from 'node:http';
import { context, createContextKey, defaultTextMapGetter, defaultTextMapSetter, ROOT_CONTEXT, SpanKind, SpanStatusCode, trace, type Context, type Span } from '@opentelemetry/api';
import { AsyncLocalStorageContextManager } from '@opentelemetry/context-async-hooks';
import { W3CTraceContextPropagator } from '@opentelemetry/core';
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-proto';
import { resourceFromAttributes } from '@opentelemetry/resources';
import { BatchSpanProcessor, ParentBasedSampler, TraceIdRatioBasedSampler, TracerProvider } from '@opentelemetry/sdk-trace';
import { findSecretShapedValues } from './secret-shape.ts';
import { GoogleAuth } from 'google-auth-library';

const propagator = new W3CTraceContextPropagator();
const logLabelsKey = createContextKey('runbook.log-labels');

export function samplingRatio(value = process.env.RUNBOOK_TRACE_SAMPLE_RATIO): number {
  if (value === undefined || value.trim() === '') return 0.1;
  const ratio = Number(value);
  return Number.isFinite(ratio) && ratio >= 0 && ratio <= 1 ? ratio : 0.1;
}

export function traceSampler(ratio = samplingRatio()) {
  return new ParentBasedSampler({ root: new TraceIdRatioBasedSampler(ratio) });
}

export function initTelemetry(role: string) {
  const project = process.env.GOOGLE_CLOUD_PROJECT;
  const endpoint = process.env.OTEL_EXPORTER_OTLP_TRACES_ENDPOINT ||
    (process.env.OTEL_EXPORTER_OTLP_ENDPOINT && process.env.OTEL_EXPORTER_OTLP_ENDPOINT.replace(/\/$/, '') + '/v1/traces');
  const google = endpoint === 'https://telemetry.googleapis.com/v1/traces';
  const auth = google ? new GoogleAuth({ scopes: 'https://www.googleapis.com/auth/cloud-platform' }) : undefined;
  const exporter = endpoint ? new OTLPTraceExporter({
    url: endpoint,
    timeoutMillis: 3000,
    ...(auth && { headers: async () => Object.fromEntries(await auth.getRequestHeaders()) }),
  }) : undefined;
  const provider = new TracerProvider({
    resource: resourceFromAttributes({ 'service.name': 'runbook-runner-' + role, ...(project && { 'gcp.project_id': project }) }),
    sampler: traceSampler(),
    spanProcessors: exporter ? [new BatchSpanProcessor({ exporter, exportTimeoutMillis: 3500 })] : [],
  });
  context.setGlobalContextManager(new AsyncLocalStorageContextManager().enable());
  trace.setGlobalTracerProvider(provider);
  return provider;
}

export const currentSpan = () => trace.getSpan(context.active());
export const activeContext = () => context.active();

export function requestContext(headers: IncomingHttpHeaders): Context {
  const traceparent = headers.traceparent;
  return typeof traceparent === 'string'
    ? propagator.extract(ROOT_CONTEXT, { traceparent }, defaultTextMapGetter)
    : ROOT_CONTEXT;
}

export function traceHeaders(): Record<string, string> {
  const headers: Record<string, string> = {};
  propagator.inject(context.active(), headers, defaultTextMapSetter);
  // Only traceparent is propagated between Runbook services.
  return headers.traceparent ? { traceparent: headers.traceparent } : {};
}

export function withStep<T>(step: string, fn: () => T): T {
  return context.with(ROOT_CONTEXT.setValue(logLabelsKey, { step_run: step }), fn);
}

export function setLabels(labels: Record<string, string>) {
  const current = context.active().getValue(logLabelsKey);
  if (current && typeof current === 'object') Object.assign(current, labels);
}

export function logCorrelation(fields: Record<string, unknown>, role: string): Record<string, unknown> {
  const labels: Record<string, string> = { runner_process: role };
  const scoped = context.active().getValue(logLabelsKey) as Record<string, string> | undefined;
  for (const [key, value] of Object.entries({
    org: fields.org_id ?? scoped?.org ?? process.env.RUNNER_ORG_ID,
    project: fields.project_id ?? scoped?.project,
    run: fields.run_id ?? scoped?.run,
    step_run: fields.step_run_id ?? scoped?.step_run,
  })) {
    if (typeof value === 'string' && identifier(value)) labels[key] = value;
  }
  const active = trace.getSpanContext(context.active());
  const project = process.env.GOOGLE_CLOUD_PROJECT;
  return {
    'logging.googleapis.com/labels': labels,
    ...(active && trace.isSpanContextValid(active) && {
      'logging.googleapis.com/trace': project ? `projects/${project}/traces/${active.traceId}` : active.traceId,
      'logging.googleapis.com/spanId': active.spanId,
      'logging.googleapis.com/trace_sampled': Boolean(active.traceFlags & 1),
    }),
  };
}

export const clientKind = SpanKind.CLIENT;
export const serverKind = SpanKind.SERVER;

/** Names are fixed operation names. Never record exceptions, URLs, payloads or headers. */
export function inSpan<T>(
  name: string,
  fn: (span: Span) => Promise<T>,
  kind = SpanKind.INTERNAL,
  parent = context.active(),
): Promise<T> {
  return trace.getTracer('runbook-runner').startActiveSpan(name, { kind }, parent, (span) =>
    span.isRecording() ? finishSpan(span, fn) : fn(span));
}

async function finishSpan<T>(span: Span, fn: (span: Span) => Promise<T>): Promise<T> {
  try {
    return await fn(span);
  } catch (error) {
    span.setStatus({ code: SpanStatusCode.ERROR });
    throw error;
  } finally {
    span.end();
  }
}

/** A span whose lifetime is managed by its caller, with a context for its children. */
export function scopedSpan(name: string, parent: Context, kind = SpanKind.INTERNAL) {
  const span = trace.getTracer('runbook-runner').startSpan(name, { kind }, parent);
  return { span, context: trace.setSpan(parent, span) };
}

export function quietRequest(path: string): boolean {
  return /^(?:\/health\/|\/(?:healthz|readyz|metrics)$|\/internal\/(?:admission|wake|capability|slots\/[^/]+\/heartbeat)$)/.test(path);
}

export function tracedServer(handler: RequestListener): RequestListener {
  return (req, res) => {
    const path = req.url?.split('?')[0] ?? '';
    // Sandbox traffic checks current egress policy on every request and CONNECT.
    if (quietRequest(path) || path === '/internal/egress/policy') {
      handler(req, res);
      return;
    }
    const parent = requestContext(req.headers).setValue(logLabelsKey, {});
    const server = scopedSpan('http.server', parent, SpanKind.SERVER);
    const { span } = server;
    res.once('close', () => {
      if (res.headersSent) span.setAttribute('http.response.status_code', res.statusCode);
      if (!res.writableFinished || res.statusCode >= 500) span.setStatus({ code: SpanStatusCode.ERROR });
      span.end();
    });
    context.with(server.context, handler, undefined, req, res);
  };
}

export const fetchWithTrace: typeof fetch = (input, init) => traceFetch(fetch, input, init);

export function traceFetch(fetcher: typeof fetch, input: Parameters<typeof fetch>[0], init?: RequestInit, name = 'http.client'): Promise<Response> {
  return httpClient(name, () => {
    const { traceparent } = traceHeaders();
    if (!traceparent) return fetcher(input, init);
    const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
    headers.set('traceparent', traceparent);
    return fetcher(input, { ...init, headers });
  });
}

export function stripTraceHeaders(headers: Headers): void {
  for (const name of ['traceparent', 'tracestate', 'baggage']) headers.delete(name);
}

/** Record third-party calls locally without passing context to the destination. */
export const fetchExternal: typeof fetch = (input, init) => httpClient('http.client', () => {
  const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
  stripTraceHeaders(headers);
  return fetch(input, { ...init, headers });
});

function httpClient(name: string, request: () => Promise<Response>): Promise<Response> {
  return inSpan(name, async (span) => {
    const response = await request();
    span.setAttribute('http.response.status_code', response.status);
    if (response.status >= 400) span.setStatus({ code: SpanStatusCode.ERROR });
    return response;
  }, SpanKind.CLIENT);
}

export function identifier(value: string): string | undefined {
  // Fixed-width and conservative names, with known token shapes rejected.
  return /^[a-zA-Z0-9][a-zA-Z0-9_./:-]{0,127}$/.test(value) &&
    findSecretShapedValues(value).length === 0 ? value : undefined;
}
