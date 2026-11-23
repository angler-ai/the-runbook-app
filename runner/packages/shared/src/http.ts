// A small JSON router over node:http for the runner's internal APIs. Every
// route names the processes allowed to call it; callers authenticate with
// their process token (constant-time compare). Bodies are JSON with a size
// limit; error answers carry a fixed code and message and nothing else.
import { timingSafeEqual } from 'node:crypto';
import http from 'node:http';
import { once } from 'node:events';
import { DECISION_CONTRACT, DECISION_CONTRACT_HEADER } from './contracts/core.ts';
import type { RunnerProcess } from './contracts/internal.ts';
import { quietRequest, setLabels, tracedServer } from './telemetry.ts';
import { errorFields, type Logger } from './log.ts';

export class HttpError extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = 'HttpError';
    this.status = status;
    this.code = code;
  }
}

export interface RouteContext {
  caller: RunnerProcess;
  params: Record<string, string>;
  runnerId?: string;
  body: unknown;
  rawBody?: Buffer;
  request?: { method: string; headers: http.IncomingHttpHeaders; signal?: AbortSignal };
}

export interface Route {
  method: string;
  /** Path template with `{name}` segments. */
  path: string;
  callers: readonly RunnerProcess[];
  maxBodyBytes?: number;
  handle(ctx: RouteContext): Promise<{ status?: number; body?: unknown; headers?: Record<string, string> }>;
}

export interface ServerOptions {
  routes: readonly Route[];
  /** Accepted tokens by calling process. */
  inboundTokens: Partial<Record<RunnerProcess, string>>;
  workerTokens?: Readonly<Record<string, string>>;
  log: Logger;
  maxBodyBytes?: number;
  decisionSupport?: boolean;
  /** Map a thrown error to an answer; return undefined for a 500. */
  mapError?(error: unknown): HttpError | undefined;
}

export function sameToken(a: string, b: string): boolean {
  const x = Buffer.from(a);
  const y = Buffer.from(b);
  return x.length === y.length && timingSafeEqual(x, y);
}

function match(template: string, path: string): Record<string, string> | undefined {
  const want = template.split('/');
  const got = path.split('/');
  const wildcard = want.at(-1) === '*';
  if (wildcard ? got.length < want.length : want.length !== got.length) return undefined;
  const params: Record<string, string> = {};
  for (let i = 0; i < want.length; i++) {
    const w = want[i] ?? '';
    const g = got[i] ?? '';
    if (wildcard && w === '*' && i === want.length - 1) {
      try {
        params['*'] = got.slice(i).map(decodeURIComponent).join('/');
      } catch {
        return undefined;
      }
    } else if (w.startsWith('{') && w.endsWith('}')) {
      if (!/^[A-Za-z0-9_.:-]{1,128}$/.test(g)) return undefined;
      params[w.slice(1, -1)] = g;
    } else if (w !== g) return undefined;
  }
  return params;
}

async function readBody(req: http.IncomingMessage, limit: number): Promise<{ body?: unknown; rawBody: Buffer }> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    size += (chunk as Buffer).length;
    if (size > limit) throw new HttpError(413, 'request.too_large', 'the request body is too large');
    chunks.push(chunk as Buffer);
  }
  const rawBody = Buffer.concat(chunks);
  if (size === 0) return { rawBody };
  const type = req.headers['content-type'] ?? '';
  if (!type.startsWith('application/json')) throw new HttpError(415, 'request.not_json', 'send application/json');
  try {
    return { rawBody, body: JSON.parse(rawBody.toString('utf8')) as unknown };
  } catch {
    throw new HttpError(400, 'request.invalid_json', 'the request body is not valid JSON');
  }
}

export function createJsonServer(options: ServerOptions): http.Server {
  return http.createServer(createJsonHandler(options));
}

export function createJsonHandler(options: ServerOptions): http.RequestListener {
  const limit = options.maxBodyBytes ?? 256 * 1024;
  return tracedServer(async (req, res) => {
    const started = Date.now();
    const url = new URL(req.url ?? '/', 'http://internal');
    if (options.decisionSupport && req.method === 'GET' && req.url === '/health/decision') {
      res.writeHead(204, { [DECISION_CONTRACT_HEADER]: String(DECISION_CONTRACT), 'cache-control': 'no-store' });
      res.end();
      return;
    }
    const requestAbort = new AbortController();
    res.once('close', () => { if (!res.writableFinished) requestAbort.abort(); });
    let matchedPath = '[unmatched]';
    const logResponse = (status: number) => {
      const method = req.method && http.METHODS.includes(req.method) ? req.method : 'OTHER';
      const fields = { method, path: matchedPath, status };
      if (status >= 400) options.log.warn('request', fields);
      else if (!quietRequest(url.pathname)) options.log.debug('request', { ...fields, ms: Date.now() - started });
    };
    const send = (status: number, body: unknown, headers?: Record<string, string>) => {
      if (res.destroyed) return;
      const text = body === undefined ? '' : JSON.stringify(body);
      res.writeHead(status, {
        'content-type': 'application/json',
        'cache-control': 'no-store',
        'x-content-type-options': 'nosniff',
        ...headers,
      });
      res.end(text);
      logResponse(status);
    };
    try {
      let params: Record<string, string> | undefined;
      const route = options.routes.find(
        (r) => (r.method === req.method || r.method === 'ALL') && (params = match(r.path, url.pathname)) !== undefined,
      );
      if (!route || !params) throw new HttpError(404, 'route.not_found', 'no such route');
      matchedPath = route.path;
      const auth = req.headers.authorization ?? '';
      const token = auth.startsWith('Bearer ') ? auth.slice(7) : '';
      const runnerId = Object.entries(options.workerTokens ?? {}).find(([, expected]) => token && sameToken(token, expected))?.[0];
      const caller = runnerId ? 'worker' : (Object.entries(options.inboundTokens) as [RunnerProcess, string][]).find(
        ([, t]) => token !== '' && sameToken(token, t),
      )?.[0];
      if (!caller) throw new HttpError(401, 'auth.invalid_token', 'missing or invalid process token');
      if (!route.callers.includes(caller)) throw new HttpError(403, 'auth.wrong_caller', 'this process may not call this route');
      const { body, rawBody } = await readBody(req, route.maxBodyBytes ?? limit);
      if (params.step_run_id) setLabels({ step_run: params.step_run_id });
      const result = await route.handle({ caller, params, body, rawBody, runnerId, request: { method: req.method ?? 'GET', headers: req.headers, signal: requestAbort.signal } });
      if (result.body instanceof ReadableStream) {
        const reader = result.body.getReader();
        const cancel = () => { void reader.cancel().catch(() => undefined); };
        requestAbort.signal.addEventListener('abort', cancel, { once: true });
        try {
          requestAbort.signal.throwIfAborted();
          res.writeHead(result.status ?? 200, { 'cache-control': 'no-store', ...result.headers });
          logResponse(res.statusCode);
          res.flushHeaders();
          while (true) {
            const part = await reader.read();
            if (part.done) break;
            if (!res.write(part.value)) await once(res, 'drain', { signal: requestAbort.signal });
          }
          res.end();
        } catch {
          cancel();
          res.destroy();
        } finally {
          requestAbort.signal.removeEventListener('abort', cancel);
        }
        return;
      }
      send(result.status ?? 200, result.body, result.headers);
    } catch (error) {
      const mapped = error instanceof HttpError ? error : options.mapError?.(error);
      if (mapped) {
        send(mapped.status, { error: { code: mapped.code, message: mapped.message } });
      } else {
        options.log.error('request failed', { path: matchedPath, ...errorFields(error) });
        send(500, { error: { code: 'internal', message: 'internal error' } });
      }
    }
  });
}
