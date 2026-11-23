// Internal routes for the Rust host (D44). Never proxied: Rust forwards only
// /api/auth/* to this service, binds it to loopback and holds the per-boot secret.
// Every /internal route needs `x-runbook-internal-secret`; without it, 401.
// Every answer (here and on /api/auth) to a request carrying
// `x-runbook-auth-nonce` carries `x-runbook-auth-proof` (see proofFor).
//
//   POST /internal/session   Cookie: <a Cookie header carrying the session cookie>
//     200 {auth_user_id, email, email_verified, name?, image?, session_id, expires_at}
//         (expires_at: integer milliseconds since the Unix epoch)
//     401 {code: "auth.internal_unauthorized" | "auth.no_session", message}
//   GET  /internal/health
//     200 {status: "ok", methods: ["email", "google"]}
//   POST /internal/oauth/introspect   {"token": "trb_oat_...", "resource": "<origin>/mcp"}   (MCP2)
//     200 {auth_user_id, email, email_verified, session_id, expires_at, scope, client_id}
//         (the /internal/session shape; session_id names the token)
//     401 {code: "auth.no_session"}   unknown, expired, revoked, malformed, or
//         minted for another resource
import type { Hono } from 'hono';
import type { AppDeps, Auth } from './app';
import { deliverMfaNotifications, mfaEnrolled, sessionFacts } from './mfa';
import { migrateCookieHeader } from './cookies';
import type { OAuthAuth } from './oauth';
import type { AuthConfig } from './config';
import { INTERNAL_SECRET_HEADER } from './hooks';

const unauthorized = (code: string, message: string) =>
  new Response(JSON.stringify({ code, message }), { status: 401, headers: { 'content-type': 'application/json' } });

async function digest(value: string): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value)));
}

/** Compares SHA-256 digests in constant time, so neither length nor prefix leaks. */
export async function secretMatches(presented: string | null | undefined, expected: string): Promise<boolean> {
  if (!presented) return false;
  const [a, b] = await Promise.all([digest(presented), digest(expected)]);
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

/** Gate fix 2: the header the native host sends on every call, and the answer. */
export const NONCE_HEADER = 'x-runbook-auth-nonce';
export const PROOF_HEADER = 'x-runbook-auth-proof';
const NONCE = /^[0-9a-f]{16,128}$/;

/**
 * Lowercase hex of HMAC-SHA-256(secret, `runbook-auth-proof\n` + nonce): proof that
 * this answer comes from the process that holds the per-boot secret, so a
 * listener that squats a previous child's port cannot answer for it
 * (`crates/runbook-server/src/auth_child.rs`, `proof_for`). Null for a
 * missing or malformed nonce.
 */
export async function proofFor(secret: string, nonce: string | null | undefined): Promise<string | null> {
  if (!secret || !nonce || !NONCE.test(nonce)) return null;
  const key = await crypto.subtle.importKey('raw', new TextEncoder().encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  const mac = await crypto.subtle.sign('HMAC', key, new TextEncoder().encode(`runbook-auth-proof\n${nonce}`));
  return Array.from(new Uint8Array(mac), (b) => b.toString(16).padStart(2, '0')).join('');
}

export interface InternalMail {
  transport: import('./email').MailTransport;
  from: string;
}

export function mountInternal(
  app: Hono,
  config: AuthConfig,
  auth: Auth | null,
  deps: AppDeps,
  mail: InternalMail | null = null,
): void {
  app.use('/internal/*', async (c, next) => {
    if (!(await secretMatches(c.req.header(INTERNAL_SECRET_HEADER), config.internalSecret))) {
      return unauthorized('auth.internal_unauthorized', 'Internal secret required.');
    }
    await next();
  });

  app.get('/internal/health', (c) => c.json({ status: 'ok', methods: config.methods }));

  app.post('/internal/session', async (c) => {
    const cookie = migrateCookieHeader(c.req.header('cookie'), config.secureCookies) ?? c.req.header('cookie');
    if (!auth || !cookie) return unauthorized('auth.no_session', 'No live session.');
    let result: Awaited<ReturnType<Auth['api']['getSession']>> = null;
    try {
      result = await auth.api.getSession({ headers: new Headers({ cookie }) });
    } catch {
      result = null;
    }
    if (!result) return unauthorized('auth.no_session', 'No live session.');
    return c.json(await sessionFacts(auth, result));
  });

  // Re-check 2: a backend failure is 503 (Rust answers it as unavailable),
  // never "not active", which would make clients discard good tokens.
  const unavailable = () => new Response(JSON.stringify({ code: 'auth.unavailable', message: 'The sign-in service could not answer.' }), { status: 503, headers: { 'content-type': 'application/json' } });
  const oauth = auth as unknown as OAuthAuth | null;
  const body = async (c: { req: { json(): Promise<unknown> } }) => {
    try {
      const value = await c.req.json();
      return value && typeof value === 'object' ? (value as Record<string, unknown>) : null;
    } catch {
      return null;
    }
  };

  app.post('/internal/oauth/introspect', async (c) => {
    const inactive = () => unauthorized('auth.no_session', 'No active access token.');
    const { token, resource } = (await body(c)) ?? {};
    if (!oauth || typeof token !== 'string' || token.length > 512 || typeof resource !== 'string' || resource.length > 512) return inactive();
    try {
      const found = await oauth.api.runbookIntrospect({ body: { token, resource } });
      return found ? c.json(found) : inactive();
    } catch {
      return unavailable();
    }
  });

  //   POST /internal/email/send            {to, subject, text, html} -> 204 (NOTIFY1)
  //   POST /internal/oauth/grants          {"auth_user_ids": [...]}  -> {grants: [...]}
  //   POST /internal/oauth/grants/revoke   {"auth_user_ids": [...], "client_id"} -> {revoked}
  // Re-check 1: a person's connected apps, for Rust's /v1/me/apps.
  // NOTIFY1: the Worker's mail sender. The body is a rendered message
  // from a Rust outbox row; the transport is this service's own (SMTP,
  // the Email binding, or capture in tests). No content is logged.
  app.post('/internal/mfa/enrollment', async (c) => {
    const { auth_user_ids } = (await body(c)) ?? {};
    if (!auth) return unavailable();
    if (!Array.isArray(auth_user_ids) || auth_user_ids.length === 0 || auth_user_ids.length > 64
      || auth_user_ids.some((id) => typeof id !== 'string' || id.length === 0 || id.length > 256)) {
      return c.json({ code: 'auth.invalid_request' }, 400);
    }
    try {
      for (const id of new Set<string>(auth_user_ids)) {
        if (await mfaEnrolled(auth, { id })) return c.json({ enrolled: true });
      }
      return c.json({ enrolled: false });
    } catch {
      return unavailable();
    }
  });
  //   POST /internal/mfa/factors   {"auth_user_ids": [...]}   (1–256 IDs)
  //     200 {factors: [{auth_user_id, email, passkeys, authenticator}]}
  // Enrolled factors per auth user, for `/v1/me/security` and the
  // organization policy preview. Unknown IDs are left out.
  app.post('/internal/mfa/factors', async (c) => {
    const { auth_user_ids } = (await body(c)) ?? {};
    if (!auth) return unavailable();
    if (!Array.isArray(auth_user_ids) || auth_user_ids.length === 0 || auth_user_ids.length > 256
      || auth_user_ids.some((id) => typeof id !== 'string' || id.length === 0 || id.length > 256)) {
      return c.json({ code: 'auth.invalid_request' }, 400);
    }
    try {
      return c.json({ factors: await deps.store.mfaFactors([...new Set<string>(auth_user_ids)]) });
    } catch {
      return unavailable();
    }
  });
  app.post('/internal/mfa/reset', async (c) => {
    const { auth_user_ids, actor } = (await body(c)) ?? {};
    if (!Array.isArray(auth_user_ids) || auth_user_ids.length === 0 || auth_user_ids.length > 64
      || auth_user_ids.some((id) => typeof id !== 'string' || id.length === 0 || id.length > 256)
      || typeof actor !== 'string' || actor.length === 0 || actor.length > 256) {
      return c.json({ code: 'auth.invalid_request' }, 400);
    }
    try {
      const found = await deps.store.resetMfa(null, [...new Set(auth_user_ids)], actor);
      for (const user of found) await deliverMfaNotifications(deps, user.id);
      return c.json({ reset: found.length > 0 });
    } catch {
      return unavailable();
    }
  });
  app.post('/internal/email/send', async (c) => {
    const found = await body(c);
    if (!mail) return unavailable();
    const { to, subject, text, html } = found ?? {};
    if (
      typeof to !== 'string' || !to.includes('@') || to.length > 254 ||
      typeof subject !== 'string' || subject.length === 0 || subject.length > 300 ||
      typeof text !== 'string' || text.length > 64 * 1024 ||
      typeof html !== 'string' || html.length > 128 * 1024
    ) {
      return c.json({ error: 'invalid_message' }, 400);
    }
    try {
      await mail.transport.send({ from: mail.from, to, subject, text, html }, '', 'notification');
      return c.body(null, 204);
    } catch {
      return unavailable();
    }
  });
  app.post('/internal/oauth/grants', async (c) => {
    const found = await body(c);
    if (!oauth) return c.json({ grants: [] });
    try {
      return c.json(await oauth.api.runbookGrants({ body: { auth_user_ids: found?.auth_user_ids } }));
    } catch {
      return unavailable();
    }
  });
  app.post('/internal/oauth/grants/revoke', async (c) => {
    const found = await body(c);
    if (!oauth) return c.json({ revoked: false });
    try {
      return c.json(await oauth.api.runbookRevokeGrant({ body: { auth_user_ids: found?.auth_user_ids, client_id: found?.client_id } }));
    } catch {
      return unavailable();
    }
  });
}
