// The one place a refused request waits for a second-factor check (J-3).
// The API client calls `requestStepUp` on `403 auth.mfa_step_up` or
// `auth.mfa_required`; the app registers a handler that shows the inline
// prompt (features/mfa/StepUpHost) and resolves true once the person has
// verified. Requests refused at the same time share one prompt.
//
// The auth service's own security routes (`/api/auth/passkey/*`,
// `/api/auth/two-factor/*`) answer `auth.mfa_step_up` too; their callers use
// the same function, so every refusal ends in the same prompt.

export interface StepUpRequest {
  /** `auth.mfa_step_up` (stale check) or `auth.mfa_required` (partial session). */
  code: string;
  /** The HTTP method of the refused request. */
  method: string;
  /** The organization's fresh-check window, when the refusal names it. */
  freshMinutes: number | null;
  /** The problem body's `security` view (partial sessions), when present. */
  security: Record<string, unknown> | null;
}

export type StepUpHandler = (request: StepUpRequest) => Promise<boolean>;

let handler: StepUpHandler | null = null;
let pending: Promise<boolean> | null = null;

/** The app registers its prompt here; returns the unregister function. */
export function registerStepUpHandler(next: StepUpHandler): () => void {
  handler = next;
  return () => {
    if (handler === next) {
      handler = null;
      pending = null;
    }
  };
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/** Normalize a refusal body (core problem+json or auth-service JSON). */
export function stepUpRequestFrom(
  code: string,
  body: Record<string, unknown>,
  method: string,
): StepUpRequest {
  const minutes = body.fresh_minutes;
  return {
    code,
    method: method.toUpperCase(),
    freshMinutes: typeof minutes === 'number' && Number.isFinite(minutes) ? minutes : null,
    security: asRecord(body.security),
  };
}

/**
 * Ask the person for a second factor. Resolves false when no prompt is
 * registered, or the person cancels; the caller then surfaces the refusal.
 */
export function requestStepUp(
  error: { code: string; body?: Record<string, unknown> },
  request: { method: string },
): Promise<boolean> {
  if (!handler) return Promise.resolve(false);
  if (pending) return pending;
  const current = handler;
  pending = current(stepUpRequestFrom(error.code, error.body ?? {}, request.method)).finally(() => {
    pending = null;
  });
  return pending;
}

/** The current cookie principal, read without the API client's retry middleware. */
export async function readStepUpPrincipal(origin: string): Promise<string | null> {
  try {
    const response = await fetch(new URL('/v1/me', origin), {
      credentials: 'same-origin',
      cache: 'no-store',
      redirect: 'error',
    });
    if (!response.ok) return null;
    const body = await response.json() as { user?: { id?: unknown }; org?: { id?: unknown } };
    const user = body.user?.id;
    const org = body.org?.id;
    return typeof user === 'string' && typeof org === 'string' ? JSON.stringify([user, org]) : null;
  } catch {
    return null;
  }
}
