// The core server's contract as seen from the runner: types generated from
// api/openapi.draft.json (`npm run gen:core`, same openapi-typescript version
// as web/) for the routes that exist today, and the hand-written R0a types in
// contracts/core.ts for the runner routes the core is adding.
import type { components, operations, paths } from './generated/core-openapi.ts';

export type { components, operations, paths };

type JsonBody<T> = T extends { content: { 'application/json': infer B } } ? B : never;
type RequestOf<O> = O extends { requestBody?: infer R } ? JsonBody<NonNullable<R>> : never;
type OkOf<O> = O extends { responses: { 200: infer R } } ? JsonBody<R> : never;

export type ClaimOperation = operations['post_v1_step-runs__sr__claim'];
export type ClaimRequest = RequestOf<ClaimOperation>;
export type ClaimResponse = OkOf<ClaimOperation>;
export type HeartbeatOperation = operations['post_v1_step-runs__sr__heartbeat'];
export type HeartbeatRequest = RequestOf<HeartbeatOperation>;
export type ReportOperation = operations['post_v1_step-runs__sr__report'];
export type ReportRequest = RequestOf<ReportOperation>;

/** A problem response from the core (RFC 9457 with a `code`). */
export interface CoreProblem {
  status: number;
  code: string;
}

export class CoreError extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, code: string) {
    // The code and status only: problem bodies can quote request values.
    super(`core answered ${status} ${code}`);
    this.name = 'CoreError';
    this.status = status;
    this.code = code;
  }
}

export interface CoreFetchOptions {
  baseUrl: string;
  token: string;
  fetch?: typeof fetch;
  timeoutMs?: number;
  signal?: AbortSignal;
  idempotencyKey?: string;
}

/** POST or GET JSON to the core with a runner token; throws CoreError on non-2xx. */
export async function coreJson<T>(
  options: CoreFetchOptions,
  method: 'GET' | 'POST' | 'PUT' | 'DELETE',
  path: string,
  body?: unknown,
): Promise<T> {
  const doFetch = options.fetch ?? fetch;
  const timeout = AbortSignal.timeout(options.timeoutMs ?? 10_000);
  const res = await doFetch(options.baseUrl + path, {
    method,
    headers: {
      authorization: `Bearer ${options.token}`,
      accept: 'application/json',
      ...(options.idempotencyKey && { 'idempotency-key': options.idempotencyKey }),
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
    },
    body: body === undefined ? null : JSON.stringify(body),
    redirect: 'error',
    signal: options.signal ? AbortSignal.any([options.signal, timeout]) : timeout,
  });
  const text = await res.text();
  if (!res.ok) {
    let code = 'core.error';
    try {
      const parsed = JSON.parse(text) as { code?: unknown; error?: { code?: unknown } };
      const c = parsed.code ?? parsed.error?.code;
      if (typeof c === 'string' && /^[a-z0-9_.-]{1,64}$/.test(c)) code = c;
    } catch {
      // Not JSON: keep the generic code.
    }
    throw new CoreError(res.status, code);
  }
  return (text === '' ? undefined : JSON.parse(text)) as T;
}
