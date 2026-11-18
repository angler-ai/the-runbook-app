// Token-paste sign-in (A3, F90, F96). The candidate token is probed with
// GET /v1/me on a throwaway client and stored only if it is a live,
// narrow, human PAT. Nothing here logs or persists a refused token.
import { ApiError, createApiRuntime, type Schemas } from '../../api/client';
import { endSession } from '../../api/session-end';
import { sessionStore, tokenStore } from '../../api/token-store';
// The tab's actor session id and claims are also cached in memory;
// features/execution/session.ts forgets them when a session ends (U1).

export type MeView = Schemas['MeView'];

export type RefusalKind =
  'service' | 'agent' | 'expired' | 'too-wide' | 'insecure' | 'invalid' | 'unavailable';

export interface SignInRefusal {
  kind: RefusalKind;
  message: string;
}

/** The narrowest useful browser token: read, start and act on runs (§0, U3). */
export const NARROW_TOKEN_SUGGESTION =
  'runbook token create --name browser --scope runbooks:read --scope executions:read --scope executions:start --scope executions:act --expires 7d';

export const OPTIONAL_SCOPES_NOTE =
  'Add --scope executions:admin to pause, resume or cancel runs, and --scope runbooks:write --scope runbooks:publish to edit runbooks.';

/**
 * F90, U3: the only scopes a browser token may hold. An allow-list, so a
 * scope the server adds later is refused until someone decides otherwise.
 * `executions:admin` is here because pause, resume and cancel need it
 * (D27); token, webhook and project management stay out of the browser.
 */
export const BROWSER_SCOPES: ReadonlySet<string> = new Set([
  'runbooks:read',
  'runbooks:write',
  'runbooks:publish',
  'executions:read',
  'executions:start',
  'executions:act',
  'executions:admin',
  'skills:read',
]);

/** Pure refusal check over a /v1/me response; null means acceptable. */
export function validateMeForSignIn(me: MeView, nowMs: number): SignInRefusal | null {
  const credential = me.credential;
  if (credential.kind !== 'personal' || !me.user) {
    return {
      kind: 'service',
      message: `That is a service token, and the web app signs in people, not projects. Paste a personal access token instead; mint one with ${NARROW_TOKEN_SUGGESTION}.`,
    };
  }
  if (credential.actor_kind === 'agent') {
    return {
      kind: 'agent',
      message: `That is an agent-kind token, and the web app signs in humans. Paste your own personal token, or mint one with ${NARROW_TOKEN_SUGGESTION}.`,
    };
  }
  if (credential.expires_at_ms !== null && credential.expires_at_ms <= nowMs) {
    return {
      kind: 'expired',
      message: `That token has expired. Mint a fresh one with ${NARROW_TOKEN_SUGGESTION}.`,
    };
  }
  const refused = credential.scopes.filter((scope) => !BROWSER_SCOPES.has(scope));
  if (refused.length > 0) {
    return {
      kind: 'too-wide',
      message: `That token carries ${refused.join(', ')}, which is too powerful to keep in a browser. Mint a narrower one for the web app with ${NARROW_TOKEN_SUGGESTION}. ${OPTIONAL_SCOPES_NOTE}`,
    };
  }
  return null;
}

/** F96: token paste only over https or on a loopback host. */
export function isSecurePasteContext(href: string): boolean {
  try {
    const url = new URL(href);
    if (url.protocol === 'https:') return true;
    return ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
  } catch {
    return false;
  }
}

export type AttemptResult =
  { ok: true; me: MeView; persisted: boolean } | { ok: false; refusal: SignInRefusal };

export async function attemptSignIn(
  rawToken: string,
  options: { secureContext: boolean; now?: number },
): Promise<AttemptResult> {
  const token = rawToken.trim();
  if (!options.secureContext) {
    return {
      ok: false,
      refusal: {
        kind: 'insecure',
        message:
          'This page is not on https or localhost, so pasting a token here could expose it in transit. Open the web app over https (or on this machine) and try again.',
      },
    };
  }
  if (token.length === 0) {
    return {
      ok: false,
      refusal: { kind: 'invalid', message: 'Paste your personal access token.' },
    };
  }
  // A pasted token with a line break or space inside would make the
  // Authorization header throw, which reads as "server unreachable" (U15).
  // Tokens are printable ASCII without spaces; say so precisely instead.
  if (!/^[\x21-\x7e]+$/.test(token)) {
    return {
      ok: false,
      refusal: {
        kind: 'invalid',
        message:
          'That token contains a space, line break or other character a token never has. Copy it again as one line and paste it here.',
      },
    };
  }
  // Probe with the candidate only; the session token is untouched until
  // every refusal check passes, and a 401 here must not clear a session.
  const probe = createApiRuntime({ getToken: () => token, onUnauthorized: null });
  let me: MeView;
  try {
    const { data } = await probe.client.GET('/v1/me');
    me = data as MeView;
  } catch (error) {
    if (error instanceof ApiError && (error.status === 401 || error.status === 403)) {
      return {
        ok: false,
        refusal: {
          kind: 'invalid',
          message:
            'The server did not accept that token. It may be mistyped, revoked, or expired; check it or mint a fresh one from the CLI.',
        },
      };
    }
    return {
      ok: false,
      refusal: {
        kind: 'unavailable',
        message: 'The server could not be reached or returned an unexpected error. Try again.',
      },
    };
  }
  const refusal = validateMeForSignIn(me, options.now ?? Date.now());
  if (refusal) return { ok: false, refusal };
  // Signing in over another live session (the sign-in page is reachable
  // while signed in): end that one first, so nothing of it carries over.
  if (tokenStore.get() !== null && tokenStore.get() !== token) endSession();
  const persisted = tokenStore.set(token);
  return { ok: true, me, persisted };
}

export const isCookieSession = () => sessionStore.isCookieSession();

/**
 * Whether the browser already holds a live sign-in cookie (another tab
 * signed in, or the marker was cleared). Marks the cookie session when it
 * does. Network or server trouble reads as "no".
 */
export async function detectCookieSession(): Promise<boolean> {
  try {
    const res = await fetch('/api/auth/get-session', { credentials: 'same-origin' });
    if (!res.ok) return false;
    const data: unknown = await res.json();
    const live =
      !!data && typeof data === 'object' && 'session' in data && !!(data as { session: unknown }).session;
    if (live) sessionStore.setCookieSession();
    return live;
  } catch {
    return false;
  }
}
export const isPatSession = () => sessionStore.isPatSession();

export async function sendVerificationOtp(
  email: string,
): Promise<{ ok: true } | { ok: false; message: string }> {
  try {
    const res = await fetch('/api/auth/email-otp/send-verification-otp', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        origin: globalThis.location?.origin ?? '',
        'x-runbook-csrf': '1',
      },
      body: JSON.stringify({ email: email.trim().toLowerCase(), type: 'sign-in' }),
    });
    if (res.ok) return { ok: true };
    let message = 'Failed to send verification code.';
    try {
      const data: unknown = await res.json();
      if (data && typeof data === 'object' && 'message' in data && typeof (data as { message: unknown }).message === 'string') {
        message = (data as { message: string }).message;
      }
    } catch {
      // Non-JSON error
    }
    return { ok: false, message };
  } catch {
    return {
      ok: false,
      message: 'Could not connect to the authentication service. Try again.',
    };
  }
}

export async function verifyEmailOtp(
  email: string,
  otp: string,
): Promise<{ ok: true; me: MeView } | { ok: false; message: string }> {
  try {
    const res = await fetch('/api/auth/sign-in/email-otp', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        origin: globalThis.location?.origin ?? '',
        'x-runbook-csrf': '1',
      },
      body: JSON.stringify({ email: email.trim().toLowerCase(), otp: otp.trim() }),
    });
    if (!res.ok) {
      let message = 'Invalid or expired code.';
      try {
        const data: unknown = await res.json();
        if (data && typeof data === 'object' && 'message' in data && typeof (data as { message: unknown }).message === 'string') {
          message = (data as { message: string }).message;
        }
      } catch {
        // Non-JSON error
      }
      return { ok: false, message };
    }
    sessionStore.setCookieSession();
    const probe = createApiRuntime({ getToken: () => null, onUnauthorized: null });
    const { data } = await probe.client.GET('/v1/me');
    return { ok: true, me: data as MeView };
  } catch {
    return { ok: false, message: 'Could not complete sign-in. Try again.' };
  }
}

export async function startGoogleSignIn(
  callbackURL = '/',
): Promise<{ ok: false; message: string } | void> {
  try {
    const res = await fetch('/api/auth/sign-in/social', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        origin: globalThis.location?.origin ?? '',
        'x-runbook-csrf': '1',
      },
      body: JSON.stringify({ provider: 'google', callbackURL }),
    });
    if (!res.ok) {
      let message = 'Google sign-in is unavailable.';
      try {
        const data: unknown = await res.json();
        if (data && typeof data === 'object' && 'message' in data && typeof (data as { message: unknown }).message === 'string') {
          message = (data as { message: string }).message;
        }
      } catch {
        // Non-JSON error
      }
      return { ok: false, message };
    }
    const data: unknown = await res.json();
    if (data && typeof data === 'object' && 'url' in data && typeof (data as { url: unknown }).url === 'string') {
      window.location.href = (data as { url: string }).url;
    }
  } catch {
    return { ok: false, message: 'Could not connect to authentication service.' };
  }
}


/**
 * Sign out: revoke the presenting PAT (B1.5) or end the cookie session, then end the session (U1).
 * The local session is dropped even if revocation fails; callers learn
 * from the result that the token may still be valid server-side.
 */
export async function signOut(): Promise<{ revoked: boolean }> {
  const token = tokenStore.get();
  let revoked = false;
  try {
    if (token) {
      // Its own runtime: a 401 here means "already revoked", not a
      // session-expired redirect racing the signed-out one.
      const revoker = createApiRuntime({ getToken: () => token, onUnauthorized: null });
      await revoker.client.DELETE('/v1/me/token', {});
      revoked = true;
    } else {
      // Cookie session sign out through auth service
      try {
        const res = await fetch('/api/auth/sign-out', {
          method: 'POST',
          headers: {
            'content-type': 'application/json',
            'origin': globalThis.location?.origin ?? '',
            'x-runbook-csrf': '1',
          },
          body: JSON.stringify({}),
        });
        revoked = res.ok;
      } catch {
        revoked = false;
      }
    }
  } catch (error) {
    // A 401 means the token is already invalid, which is as good as revoked.
    revoked = error instanceof ApiError && error.status === 401;
  } finally {
    endSession();
  }
  return { revoked };
}

