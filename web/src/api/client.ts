// openapi-fetch client with the A5 middleware: bearer auth, idempotency
// keys (one UUID per user action, reused on retry; D20), If-Match
// passthrough, and problem+json mapped to ApiError.
//
// Callers that can be retried pass an action id via `headers: { [ACTION_HEADER]: id }`
// (or `idempotencyKeyFor(id)` where the generated types require the
// Idempotency-Key parameter). Every retry of that action sends the same
// key until `forgetAction(id)` is called when the action completes. POSTs
// without an action id get a fresh one-shot key.
import createClient, { type Middleware } from 'openapi-fetch';
import type { components, paths } from './schema';
import { endSession } from './session-end';
import { sessionStore, tokenStore } from './token-store';

export type ApiClient = ReturnType<typeof createClient<paths>>;
export type Schemas = components['schemas'];
export type FieldError = Schemas['FieldError'];

export const ACTION_HEADER = 'x-runbook-action-id';

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly requestId: string | null;
  readonly errors: FieldError[];

  constructor(init: {
    status: number;
    code: string;
    requestId?: string | null;
    errors?: FieldError[];
    detail?: string;
  }) {
    super(init.detail ?? init.code);
    this.name = 'ApiError';
    this.status = init.status;
    this.code = init.code;
    this.requestId = init.requestId ?? null;
    this.errors = init.errors ?? [];
  }
}

function requestIdOf(response: Response, problem: Partial<Schemas['Problem']> | null) {
  const header = response.headers.get('x-request-id');
  if (header) return header;
  const instance = problem?.instance;
  if (typeof instance === 'string' && instance.startsWith('urn:request:')) {
    return instance.slice('urn:request:'.length);
  }
  return typeof instance === 'string' ? instance : null;
}

async function toApiError(response: Response): Promise<ApiError> {
  let problem: Partial<Schemas['Problem']> | null = null;
  try {
    const body: unknown = await response.clone().json();
    if (body && typeof body === 'object') problem = body as Partial<Schemas['Problem']>;
  } catch {
    problem = null; // Non-JSON error body: status-only ApiError below.
  }
  return new ApiError({
    status: response.status,
    code: typeof problem?.code === 'string' ? problem.code : `http.${response.status}`,
    detail: typeof problem?.detail === 'string' ? problem.detail : response.statusText,
    requestId: requestIdOf(response, problem),
    errors: Array.isArray(problem?.errors) ? problem.errors : [],
  });
}

export interface UnauthorizedContext {
  /** The token the refused request carried (null if it carried none). */
  token: string | null;
}

export interface ApiRuntimeOptions {
  baseUrl?: string;
  getToken?: () => string | null;
  /**
   * Called once per 401 response before the ApiError propagates, with the
   * token that request carried, so a late 401 from an old token can be
   * told apart from one for the current session (U14).
   */
  onUnauthorized?: ((error: ApiError, context: UnauthorizedContext) => void) | null;
}

export interface ApiRuntime {
  client: ApiClient;
  /** Stable idempotency key for one user action; same action, same key. */
  idempotencyKeyFor(actionId: string): string;
  /** Drop an action's key once the action is done, so a repeat is new. */
  forgetAction(actionId: string): void;
}

export function newActionId(): string {
  return globalThis.crypto.randomUUID();
}

export function createApiRuntime(options: ApiRuntimeOptions = {}): ApiRuntime {
  const getToken = options.getToken ?? (() => tokenStore.get());
  const actionKeys = new Map<string, string>();
  const idempotencyKeyFor = (actionId: string) => {
    let key = actionKeys.get(actionId);
    if (!key) {
      key = globalThis.crypto.randomUUID();
      actionKeys.set(actionId, key);
    }
    return key;
  };

  // The token each request was sent with, keyed by the Request object
  // openapi-fetch hands back to onResponse.
  const sentWith = new WeakMap<Request, string | null>();

  const middleware: Middleware = {
    onRequest({ request }) {
      const headers = new Headers(request.headers);
      const token = getToken();
      if (token) headers.set('authorization', `Bearer ${token}`);
      if (!headers.has('x-runbook-csrf')) headers.set('x-runbook-csrf', '1');
      const actionId = headers.get(ACTION_HEADER);
      if (actionId) headers.delete(ACTION_HEADER);
      if (request.method.toUpperCase() === 'POST' && !headers.has('idempotency-key')) {
        headers.set(
          'idempotency-key',
          actionId ? idempotencyKeyFor(actionId) : globalThis.crypto.randomUUID(),
        );
      }
      // If-Match is a plain passthrough: callers supply the exact ETag.
      const sent = new Request(request, { headers });
      sentWith.set(sent, token);
      return sent;
    },
    async onResponse({ request, response }) {
      if (response.ok) return undefined;
      const error = await toApiError(response);
      if (error.status === 401) {
        options.onUnauthorized?.(error, { token: sentWith.get(request) ?? null });
      }
      throw error;
    },
  };

  const client = createClient<paths>({
    // Same origin in the browser (A4); absolute so non-DOM fetch works too.
    baseUrl: options.baseUrl ?? globalThis.location?.origin ?? '',
    // Resolve fetch per request so MSW (and any later wrapper) intercepts.
    fetch: (request) => globalThis.fetch(request),
  });
  client.use(middleware);

  return {
    client,
    idempotencyKeyFor,
    forgetAction: (actionId) => {
      actionKeys.delete(actionId);
    },
  };
}

let unauthorizedHandler: (() => void) | null = null;

/** The router registers navigation to the sign-in page here (401 handling). */
export function registerUnauthorizedHandler(handler: (() => void) | null) {
  unauthorizedHandler = handler;
}

/**
 * App-wide runtime: token from the token store. A 401 ends the session
 * (endSession, U1) only if it answers the current token or active cookie session:
 * a late 401 for a token the user has since replaced must not sign it out (U14).
 */
export const apiRuntime: ApiRuntime = createApiRuntime({
  onUnauthorized: (_error, { token }) => {
    if (token !== null) {
      if (token !== tokenStore.get()) return;
    } else {
      // Cookie session: only handle if we believed we had an active cookie session
      if (sessionStore.getKind() !== 'cookie') return;
    }
    endSession();
    unauthorizedHandler?.();
  },
});

export const api: ApiClient = apiRuntime.client;
