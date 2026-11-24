// MCP2: OAuth 2.1 for remote MCP clients (PRD §11). Better Auth's OAuth provider
// plugin (@better-auth/oauth-provider, same version as better-auth) is the
// authorization server; `/mcp` in Rust is the only resource it issues tokens for.
//
//   - Dynamic client registration (RFC 7591), open: MCP clients register
//     themselves. Public clients (`none`) and confidential ones.
//   - Authorization code with PKCE, S256 only, always (the plugin refuses
//     `plain`; public clients cannot skip it, and confidential DCR clients
//     require it by default).
//   - Refresh tokens, rotated on every use, when `offline_access` is granted.
//   - Opaque access tokens (`trb_oat_`) and refresh tokens (`trb_ort_`), stored
//     hashed. No JWT plugin: Rust checks a token through the internal
//     introspection route (D44's internal call), so no JWT stack in Rust.
//   - RFC 8707: every token names the resource `{origin}/mcp`. An authorize
//     request without `resource` gets it added; another resource is
//     `invalid_target`.
//   - A minimal server-rendered sign-in (email codes) and consent page. The
//     consent lists every requested scope; `runbooks:publish` is an unticked
//     box, so it is granted only when the person ticks it.
import { APIError, createAuthEndpoint } from 'better-auth/api';
import { constantTimeEqual, makeSignature } from 'better-auth/crypto';
import { getOAuthProviderApi, oauthProvider, oauthProviderAuthServerMetadata } from '@better-auth/oauth-provider';
import { isLoopbackOrigin } from './config';

export const ACCESS_TOKEN_PREFIX = 'trb_oat_';
export const REFRESH_TOKEN_PREFIX = 'trb_ort_';
export const OFFLINE_ACCESS = 'offline_access';
export const PUBLISH = 'runbooks:publish';
/** R0-2: the runner scopes, opt-in like publishing (runner design §11.5). */
export const RUNNER_READ = 'runner:read';
export const RUNNER_ADMIN = 'runner:admin';
/** Scopes granted only when the person ticks their box. */
export const OPT_IN: ReadonlySet<string> = new Set([PUBLISH, RUNNER_READ, RUNNER_ADMIN]);

/**
 * The scopes an MCP client may ask for, in consent order, with the consent
 * page's wording. Only what the MCP tools use: no token, webhook or
 * administration scopes. Rust narrows a grant further to the person's role.
 */
export const SCOPES: ReadonlyArray<readonly [string, string]> = [
  ['runbooks:read', 'Find and read runbooks'],
  ['runbooks:write', 'Create runbooks and edit drafts'],
  [PUBLISH, 'Publish runbook versions that future runs use'],
  ['executions:read', 'Read runs, their steps and reports'],
  ['executions:start', 'Start runs'],
  ['executions:act', 'Claim, work on and report steps'],
  ['skills:read', 'Read the skills runbooks use'],
  [RUNNER_READ, "Read the organization's runner, its agents, their activity and costs"],
  [RUNNER_ADMIN, "Propose changes to the runner's agents (a person approves anything that widens access)"],
  [OFFLINE_ACCESS, 'Stay connected without signing in again'],
];
const SCOPE_NAMES = SCOPES.map(([name]) => name);

export const mcpResource = (origin: string) => `${origin}/mcp`;
const ACCESS_TOKEN_SECONDS = 3600;
/**
 * MCP2 re-check 1: a grant's absolute lifetime, counted from the person's
 * last consent to that client. Refresh tokens rotate within it; after it
 * the client must send the person through consent again.
 */
export const GRANT_MAX_MS = 90 * 24 * 3600 * 1000;
const REFRESH_TOKEN_SECONDS = 30 * 24 * 3600;

/** Params the plugin adds to a signed redirect query; not authorize params. */
const SIGNING_PARAMS = ['sig', 'exp', 'ba_iat', 'ba_pl', 'ba_param'];

type ProviderOptions = Parameters<typeof getOAuthProviderApi>[1];

/** The provider plugin and our two server-only endpoints that read it. */
export function oauthPlugins(origin: string) {
  const resource = mcpResource(origin);
  const provider = oauthProvider({
    loginPage: `${origin}/api/auth/oauth/sign-in`,
    consentPage: `${origin}/api/auth/oauth/consent`,
    scopes: [...SCOPE_NAMES],
    grantTypes: ['authorization_code', 'refresh_token'],
    allowDynamicClientRegistration: true,
    allowUnauthenticatedClientRegistration: true,
    clientRegistrationDefaultScopes: [...SCOPE_NAMES],
    disableJwtPlugin: true,
    resources: [{ identifier: resource, name: 'The Runbook MCP server' }],
    clientRegistrationDefaultResources: [resource],
    accessTokenExpiresIn: ACCESS_TOKEN_SECONDS,
    refreshTokenExpiresIn: REFRESH_TOKEN_SECONDS,
    prefix: { opaqueAccessToken: ACCESS_TOKEN_PREFIX, refreshToken: REFRESH_TOKEN_PREFIX },
    // Review fix 5: registrations are capped per client address and day by
    // the gateway (keyed like the send-code limits); the plugin's own
    // 5-a-minute rule would only pre-empt it.
    rateLimit: { register: false },
  });
  const opts = provider.options as unknown as ProviderOptions;
  type Adapter = { findOne(q: unknown): Promise<unknown>; findMany(q: unknown): Promise<unknown[]>; updateMany(q: unknown): Promise<unknown>; deleteMany(q: unknown): Promise<unknown> };
  async function currentFactors(adapter: Adapter, userId: string) {
    const where = [{ field: 'userId', value: userId }];
    const [passkey, totp] = await Promise.all([
      adapter.findOne({ model: 'passkey', where }),
      adapter.findOne({ model: 'twoFactor', where: [...where, { field: 'verified', value: true }] }),
    ]);
    return { passkey: !!passkey, totp: !!totp };
  }

  function currentProof(method: string | undefined, factors: Awaited<ReturnType<typeof currentFactors>>): boolean {
    return method === 'passkey' ? factors.passkey : (method === 'totp' || method === 'backup_code') && factors.totp;
  }

  const grantOf = async (adapter: Adapter, clientId: string, userId: string) =>
    (await adapter.findOne({
      model: 'oauthConsent',
      where: [
        { field: 'clientId', value: clientId },
        { field: 'userId', value: userId },
      ],
    })) as { id: string; scopes?: unknown; createdAt?: Date | string; updatedAt?: Date | string } | null;
  /** The grant behind a token: consented, not revoked through connected apps, within GRANT_MAX_MS. */
  const grantAlive = async (adapter: Adapter, clientId: string, userId: string) => {
    const consent = await grantOf(adapter, clientId, userId);
    const since = consent ? new Date(consent.updatedAt ?? consent.createdAt ?? 0).getTime() : 0;
    return consent !== null && Date.now() - since < GRANT_MAX_MS;
  };
  // Re-check 3: a person has a handful of auth identities; more than the cap
  // is refused rather than cut short, so a revoke is never silently partial.
  const MAX_IDS = 64;
  const ids = (value: unknown) => {
    const list = Array.isArray(value) ? value.filter((v): v is string => typeof v === 'string' && v.length > 0 && v.length <= 256) : [];
    if (list.length > MAX_IDS) throw new APIError('BAD_REQUEST', { code: 'auth.too_many_identities', message: 'Too many identities.' });
    return list;
  };
  // An explicit bound on a person's connected apps, refused when exceeded
  // rather than truncated by the adapter's default limit.
  const MAX_GRANTS = 500;
  const internals = {
    id: 'runbook-oauth-internals',
    endpoints: {
      /**
       * D44 for OAuth: what an access token is, for Rust, in the shape of
       * `/internal/session` plus `scope` and `client_id`. Rust names the
       * resource it serves; a token whose audience lacks it is not active
       * (RFC 8707), so neither is one minted for another resource.
       */
      runbookIntrospect: createAuthEndpoint('/runbook/oauth/introspect', { method: 'POST', metadata: { SERVER_ONLY: true } }, async (ctx) => {
        const { token, resource } = (ctx.body ?? {}) as { token?: unknown; resource?: unknown };
        if (typeof token !== 'string' || !token.startsWith(ACCESS_TOKEN_PREFIX) || token.length > 512 || typeof resource !== 'string') return null;
        const api = getOAuthProviderApi(ctx as never, opts);
        // Review fix 4: the grant, not the browser session that consented to
        // it, keeps a token alive. The plugin's own validation also requires
        // that session, so a token minted by refresh after the person signed
        // out of the web app (or after its session expired) would be
        // inactive. Here: the token row (expiry, revocation), its client, its
        // audience and its user. Revoking the token, reusing a spent refresh
        // token (the family) or a disabled client still ends it.
        const row = (await ctx.context.adapter.findOne({
          model: 'oauthAccessToken',
          where: [{ field: 'token', value: await api.hashToken(token.slice(ACCESS_TOKEN_PREFIX.length), 'access_token') }],
        })) as { clientId?: string; userId?: string; resources?: unknown; scopes?: unknown; expiresAt?: Date | string; revoked?: unknown; mfaVerifiedAt?: Date; mfaMethod?: string } | null;
        if (!row?.clientId || !row.userId || row.revoked || !row.expiresAt) return null;
        const expiresAt = new Date(row.expiresAt).getTime();
        if (!(expiresAt > Date.now())) return null;
        if (!Array.isArray(row.resources) || !row.resources.includes(resource)) return null;
        const client = (await api.getClient(row.clientId)) as { disabled?: boolean } | null;
        if (!client || client.disabled) return null;
        const user = await ctx.context.internalAdapter.findUserById(row.userId);
        if (!user) return null;
        // Re-check 1: a grant revoked by the person, or past its absolute
        // lifetime, ends its tokens at once.
        if (!(await grantAlive(ctx.context.adapter as unknown as Adapter, row.clientId, row.userId))) return null;
        const factors = await currentFactors(ctx.context.adapter as unknown as Adapter, user.id);
        const proof = currentProof(row.mfaMethod, factors);
        const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(token)));
        return {
          auth_user_id: user.id,
          email: user.email,
          email_verified: user.emailVerified === true,
          // Names this token (not a browser session); Rust derives the credential ID from it.
          session_id: `oauth:${[...digest.slice(0, 16)].map((b) => b.toString(16).padStart(2, '0')).join('')}`,
          expires_at: expiresAt,
          scope: Array.isArray(row.scopes) ? row.scopes.filter((x) => typeof x === 'string').join(' ') : '',
          client_id: row.clientId,
          mfa_enrolled: factors.passkey || factors.totp,
          mfa_verified_at: proof && row.mfaVerifiedAt ? new Date(row.mfaVerifiedAt).getTime() : null,
          mfa_method: proof ? row.mfaMethod : null,
        };
      }),
      /**
       * Re-check 1: may this refresh token still be used? False when its grant
       * was revoked through connected apps or is past GRANT_MAX_MS; true for
       * an unknown token (the plugin refuses that itself).
       */
      runbookRefreshAllowed: createAuthEndpoint('/runbook/oauth/refresh-allowed', { method: 'POST', metadata: { SERVER_ONLY: true } }, async (ctx) => {
        const token = (ctx.body as { refresh_token?: unknown } | undefined)?.refresh_token;
        if (typeof token !== 'string' || !token.startsWith(REFRESH_TOKEN_PREFIX) || token.length > 512) return { allowed: true };
        const adapter = ctx.context.adapter as unknown as Adapter;
        const row = (await adapter.findOne({
          model: 'oauthRefreshToken',
          where: [{ field: 'token', value: await getOAuthProviderApi(ctx as never, opts).hashToken(token.slice(REFRESH_TOKEN_PREFIX.length), 'refresh_token') }],
        })) as { clientId?: string; userId?: string; mfaMethod?: string } | null;
        if (!row?.clientId || !row.userId) return { allowed: true };
        const alive = await grantAlive(adapter, row.clientId, row.userId);
        const proofStillLive = !row.mfaMethod || currentProof(row.mfaMethod, await currentFactors(adapter, row.userId));
        return { allowed: alive && proofStillLive };
      }),
      /** Re-check 1: a person's connected apps (their consents), for Rust's `/v1/me/apps`. */
      runbookGrants: createAuthEndpoint('/runbook/oauth/grants', { method: 'POST', metadata: { SERVER_ONLY: true } }, async (ctx) => {
        const users = ids((ctx.body as { auth_user_ids?: unknown } | undefined)?.auth_user_ids);
        if (users.length === 0) return { grants: [] };
        const adapter = ctx.context.adapter as unknown as Adapter;
        const api = getOAuthProviderApi(ctx as never, opts);
        const consents = (await adapter.findMany({ model: 'oauthConsent', where: [{ field: 'userId', operator: 'in', value: users }], limit: MAX_GRANTS + 1 })) as {
          clientId: string;
          userId: string;
          scopes?: unknown;
          createdAt?: Date | string;
          updatedAt?: Date | string;
        }[];
        if (consents.length > MAX_GRANTS) throw new APIError('BAD_REQUEST', { code: 'auth.too_many_grants', message: 'Too many connected apps.' });
        const grants = [];
        for (const consent of consents) {
          const client = (await api.getClient(consent.clientId)) as { name?: string | null; redirectUris?: unknown } | null;
          const last = (await adapter.findMany({
            model: 'oauthAccessToken',
            where: [
              { field: 'clientId', value: consent.clientId },
              { field: 'userId', value: consent.userId },
            ],
            sortBy: { field: 'createdAt', direction: 'desc' },
            limit: 1,
          })) as { createdAt?: Date | string }[];
          const granted = new Date(consent.updatedAt ?? consent.createdAt ?? 0).getTime();
          grants.push({
            client_id: consent.clientId,
            client_name: client?.name ?? null,
            redirect_hosts: [...new Set((Array.isArray(client?.redirectUris) ? client.redirectUris : []).map((u) => redirectHost(String(u))))],
            scopes: Array.isArray(consent.scopes) ? consent.scopes.filter((x) => typeof x === 'string') : [],
            created_at: new Date(consent.createdAt ?? granted).getTime(),
            last_used_at: last[0]?.createdAt ? new Date(last[0].createdAt).getTime() : null,
            expires_at: granted + GRANT_MAX_MS,
          });
        }
        grants.sort((a, b) => b.created_at - a.created_at || a.client_id.localeCompare(b.client_id));
        return { grants };
      }),
      /**
       * Re-check 1: the person ends one connected app. Its consent goes and
       * every refresh and access token of that client for that person is
       * marked revoked.
       */
      runbookRevokeGrant: createAuthEndpoint('/runbook/oauth/grants/revoke', { method: 'POST', metadata: { SERVER_ONLY: true } }, async (ctx) => {
        const body = (ctx.body ?? {}) as { auth_user_ids?: unknown; client_id?: unknown };
        const users = ids(body.auth_user_ids);
        const clientId = typeof body.client_id === 'string' && body.client_id.length <= 256 ? body.client_id : null;
        if (users.length === 0 || !clientId) return { revoked: false };
        const adapter = ctx.context.adapter as unknown as Adapter;
        let found = false;
        for (const userId of users) {
          const where = [
            { field: 'clientId', value: clientId },
            { field: 'userId', value: userId },
          ];
          if (await grantOf(adapter, clientId, userId)) found = true;
          const now = new Date();
          await adapter.updateMany({ model: 'oauthRefreshToken', where, update: { revoked: now } });
          await adapter.updateMany({ model: 'oauthAccessToken', where, update: { revoked: now } });
          await adapter.deleteMany({ model: 'oauthConsent', where });
        }
        return { revoked: found };
      }),
      /** What the consent page shows, from a signed query it verifies. */
      runbookConsentView: createAuthEndpoint('/runbook/oauth/consent-view', { method: 'POST', metadata: { SERVER_ONLY: true } }, async (ctx) => {
        const query = (ctx.body as { oauth_query?: unknown } | undefined)?.oauth_query;
        if (typeof query !== 'string' || query.length > 8192) return null;
        const params = new URLSearchParams(query);
        const sigs = params.getAll('sig');
        const exp = Number(params.get('exp'));
        params.delete('sig');
        const sorted = new URLSearchParams([...params.entries()].sort(([a, x], [b, y]) => (a < b ? -1 : a > b ? 1 : x < y ? -1 : x > y ? 1 : 0)));
        const expected = await makeSignature(sorted.toString(), ctx.context.secret);
        if (sigs.length !== 1 || !constantTimeEqual(sigs[0], expected) || !(exp * 1000 >= Date.now())) return null;
        const clientId = params.get('client_id');
        if (!clientId) return null;
        const client = (await getOAuthProviderApi(ctx as never, opts).getClient(clientId)) as { name?: string | null; disabled?: boolean } | null;
        if (!client || client.disabled) return null;
        return {
          client_id: clientId,
          client_name: client.name ?? null,
          redirect_uri: params.get('redirect_uri') ?? '',
          scopes: (params.get('scope') ?? '').split(' ').filter((s) => s.length > 0),
          resources: params.getAll('resource'),
        };
      }),
    },
  };
  return [provider, internals];
}

/** `/internal/session`'s shape, plus the grant's `scope` and `client_id`. */
export interface Introspection {
  auth_user_id: string;
  email: string;
  email_verified: boolean;
  session_id: string;
  expires_at: number;
  scope: string;
  client_id: unknown;
}
export interface ConsentView {
  client_id: string;
  client_name: string | null;
  /** The registered redirect URI this authorization returns to. */
  redirect_uri: string;
  scopes: string[];
  resources: string[];
}
/** The slice of the Better Auth instance this module calls. */
export interface OAuthAuth {
  handler(request: Request): Promise<Response>;
  api: {
    getSession(input: { headers: Headers }): Promise<{ user: { email: string } } | null>;
    getOAuthServerConfig: (...args: never[]) => unknown;
    runbookIntrospect(input: { body: { token: string; resource: string } }): Promise<Introspection | null>;
    runbookConsentView(input: { body: { oauth_query: string } }): Promise<ConsentView | null>;
    runbookRefreshAllowed(input: { body: { refresh_token: string } }): Promise<{ allowed: boolean }>;
    runbookGrants(input: { body: { auth_user_ids: unknown } }): Promise<{ grants: unknown[] }>;
    runbookRevokeGrant(input: { body: { auth_user_ids: unknown; client_id: unknown } }): Promise<{ revoked: boolean }>;
  };
}

// ------------------------------------------------------------------ pages

const escape = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const PAGE_CSP = "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'";

function page(status: number, title: string, body: string, headers: HeadersInit = {}): Response {
  const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escape(title)} · The Runbook</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:32rem;margin:3rem auto;padding:0 1rem}label{display:block;margin:.5rem 0}input[type=email],input[type=text]{display:block;width:100%;box-sizing:border-box;padding:.5rem;font:inherit}button{padding:.5rem 1rem;font:inherit;margin:1rem .5rem 0 0}fieldset{border:1px solid #888;padding:.5rem 1rem}code{font-size:.9em}.error{color:#b00020}</style>
</head><body><main>
${body}
</main></body></html>`;
  const h = new Headers(headers);
  h.set('content-type', 'text/html; charset=utf-8');
  h.set('content-security-policy', PAGE_CSP);
  h.set('x-frame-options', 'DENY');
  h.set('cache-control', 'no-store');
  // Review fix 1: never `no-referrer`, which makes browsers send
  // `Origin: null` on the pages' form posts (refused, rightly, by the Origin
  // check). Same-origin keeps the signed query off every other site.
  h.set('referrer-policy', 'same-origin');
  return new Response(html, { status, headers: h });
}

const hidden = (name: string, value: string) => `<input type="hidden" name="${name}" value="${escape(value)}">`;
const error = (message: string | null) => (message ? `<p class="error" role="alert">${escape(message)}</p>` : '');

function emailStep(query: string, message: string | null, status = 200): Response {
  return page(
    status,
    'Sign in',
    `<h1>Sign in to The Runbook</h1>
<p>Sign in to connect an app to your account. We will email you a 6-digit code.</p>
${error(message)}
<form method="post" action="/api/auth/oauth/sign-in">${hidden('oauth_query', query)}
<label>Email <input type="email" name="email" required autocomplete="email"></label>
<button type="submit">Send code</button></form>`,
  );
}

function codeStep(query: string, email: string, message: string | null, status = 200): Response {
  return page(
    status,
    'Enter your code',
    `<h1>Check your email</h1>
<p>We sent a 6-digit code to <strong>${escape(email)}</strong>. It works for 10 minutes.</p>
${error(message)}
<form method="post" action="/api/auth/oauth/sign-in">${hidden('oauth_query', query)}${hidden('email', email)}
<label>Code <input type="text" name="otp" inputmode="numeric" pattern="[0-9]{6}" maxlength="6" required autocomplete="one-time-code"></label>
<button type="submit">Sign in</button></form>`,
  );
}

const expired = () =>
  page(400, 'Link expired', `<h1>This link has expired</h1><p>Go back to the app you were connecting and start again.</p>`);

/**
 * Review fix 2: a self-registered name is the registrant's claim. Control,
 * format (bidi overrides, zero-width) and line characters are removed, it is
 * cut to 64 characters, and the page always labels it unverified.
 */
const INVISIBLE = /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Default_Ignorable_Code_Point}\u115F\u1160\u3164\uFFA0\u2800]/gu;
export function displayName(name: string | null | undefined): string {
  // Re-check 8: also the Hangul and Braille blanks, default-ignorable code
  // points, and combining marks left after NFC (stacks of them).
  const clean = [
    ...(name ?? '')
      .normalize('NFC')
      .replace(INVISIBLE, '')
      .replace(/\p{Mn}+/gu, '')
      .replace(/\s+/g, ' ')
      .trim(),
  ];
  if (!clean.some((c) => /[\p{L}\p{N}\p{P}\p{S}]/u.test(c))) return 'Unnamed app';
  return clean.length > 64 ? `${clean.slice(0, 63).join('')}…` : clean.join('');
}

/** Where the person is sent after allowing: the redirect URI's host, said plainly. */
export function redirectHost(uri: string): string {
  try {
    const url = new URL(uri);
    if (url.protocol === 'http:' && isLoopbackOrigin(url.origin)) return `${url.host} (an app on this computer)`;
    return url.protocol === 'https:' || url.protocol === 'http:' ? url.host : `${url.protocol}// (an app on this computer)`;
  } catch {
    return 'an unknown address';
  }
}

function consentPage(query: string, view: ConsentView, email: string, message: string | null, status = 200): Response {
  const client = displayName(view.client_name);
  const rows = SCOPES.filter(([name]) => view.scopes.includes(name))
    .map(([name, text]) => {
      const optIn = OPT_IN.has(name);
      return `<label><input type="checkbox" name="scope" value="${name}"${optIn ? '' : ' checked'}> ${escape(text)} <code>${name}</code>${optIn ? ' (not granted unless you tick it)' : ''}</label>`;
    })
    .join('\n');
  return page(
    status,
    'Allow access',
    `<h1>Allow ${escape(client)} to use The Runbook?</h1>
<p><strong>${escape(client)}</strong> is the name the app gave itself: <strong>unverified (self-registered)</strong>.</p>
<p>If you allow, you are sent back to <strong class="host">${escape(redirectHost(view.redirect_uri))}</strong>. Only allow if you started connecting that app yourself.</p>
<p>You are signed in as <strong>${escape(email)}</strong>. The app will act as an agent for you at <code>${escape(view.resources.join(' '))}</code>, never with more than your own permissions.</p>
<p>Client ID: <code>${escape(view.client_id)}</code></p>
${error(message)}
<form method="post" action="/api/auth/oauth/consent">${hidden('oauth_query', query)}
<fieldset><legend>It asks to</legend>
${rows}
</fieldset>
<button type="submit" name="decision" value="allow">Allow</button><button type="submit" name="decision" value="deny">Deny</button></form>`,
  );
}

// ---------------------------------------------------------------- gateway

const FORM_BYTES = 16 * 1024;

async function readForm(request: Request): Promise<URLSearchParams | null> {
  if (!(request.headers.get('content-type') ?? '').toLowerCase().startsWith('application/x-www-form-urlencoded')) return null;
  if (Number(request.headers.get('content-length') ?? '0') > FORM_BYTES) return null;
  const text = await request.text();
  return text.length > FORM_BYTES ? null : new URLSearchParams(text);
}

/** The authorize query from a signed redirect query, with the signing params removed. */
function authorizeQuery(signed: string): string {
  const params = new URLSearchParams(signed);
  for (const name of SIGNING_PARAMS) params.delete(name);
  // Re-check 6: the person has just signed in, so `prompt=login` is met;
  // keeping it would send them back to the sign-in page forever.
  const prompts = (params.get('prompt') ?? '').split(' ').filter((p) => p && p !== 'login');
  if (prompts.length > 0) params.set('prompt', prompts.join(' '));
  else params.delete('prompt');
  return params.toString();
}

const see = (location: string, headers: Headers = new Headers()) => {
  headers.set('location', location);
  headers.set('cache-control', 'no-store');
  headers.set('referrer-policy', 'same-origin');
  return new Response(null, { status: 303, headers });
};

const oauthError = (status: number, error: string, description: string) =>
  new Response(JSON.stringify({ error, error_description: description }), { status, headers: { 'content-type': 'application/json', 'cache-control': 'no-store' } });

/** Machine-to-machine OAuth endpoints: no Origin check, and never a cookie. */
export const CLIENT_ENDPOINTS = new Set(['POST /oauth2/token', 'POST /oauth2/register', 'POST /oauth2/revoke', 'POST /oauth2/introspect']);
/** Browser OAuth routes: Better Auth's authorize, and our two pages. */
export const BROWSER_ROUTES = new Set(['GET /oauth2/authorize', 'GET /oauth/sign-in', 'POST /oauth/sign-in', 'GET /oauth/consent', 'POST /oauth/consent']);

export interface GatewayDeps {
  origin: string;
  /**
   * Review fix 5: a registration from `ip` may proceed (a daily per-address
   * cap); also sweeps clients that never completed a grant.
   */
  admitRegistration(ip: string | null): Promise<boolean>;
  emailEnabled: boolean;
  clientIpHeader: string;
  auth: OAuthAuth;
  /** This service's own gateway, for the sign-in calls (its limits and checks apply). */
  self(request: Request): Promise<Response>;
}

/** A client endpoint: forwarded with a bounded body and without cookies. */
export async function clientEndpoint(deps: GatewayDeps, raw: Request): Promise<Response> {
  if (Number(raw.headers.get('content-length') ?? '0') > FORM_BYTES) return oauthError(413, 'invalid_request', 'Request body too large.');
  let body = await raw.text();
  if (body.length > FORM_BYTES) return oauthError(413, 'invalid_request', 'Request body too large.');
  const path = new URL(raw.url).pathname;
  if (path.endsWith('/oauth2/token')) {
    // Re-check 1: a refresh only within a live grant (not revoked by the
    // person, not past its absolute lifetime).
    const form = new URLSearchParams(body);
    // Re-check 3: a single-valued parameter sent twice is refused (RFC 6749
    // §3.2), so the gate can't read one value while the plugin acts on
    // another. `resource` may repeat (RFC 8707).
    for (const key of ['grant_type', 'refresh_token', 'code', 'code_verifier', 'client_id', 'redirect_uri', 'scope']) {
      if (form.getAll(key).length > 1) return oauthError(400, 'invalid_request', `The ${key} parameter is repeated.`);
    }
    const refresh = form.get('refresh_token');
    if (form.get('grant_type') === 'refresh_token' && refresh) {
      const { allowed } = await deps.auth.api.runbookRefreshAllowed({ body: { refresh_token: refresh } });
      if (!allowed) return oauthError(400, 'invalid_grant', 'This grant has ended; authorize again.');
    }
  }
  if (path.endsWith('/oauth2/register')) {
    // Re-check 7: the per-address limit needs an address; without one there
    // is no bucket to count in, so the registration is refused.
    if (!raw.headers.get(deps.clientIpHeader)) return oauthError(400, 'invalid_request', 'The client address is unknown.');
    const checked = registration(body);
    if ('error' in checked) return oauthError(400, 'invalid_client_metadata', checked.error);
    if (!(await deps.admitRegistration(raw.headers.get(deps.clientIpHeader)))) {
      return new Response(JSON.stringify({ error: 'too_many_requests', error_description: 'Too many client registrations from this address today.' }), {
        status: 429,
        headers: { 'content-type': 'application/json', 'cache-control': 'no-store', 'retry-after': '3600' },
      });
    }
    body = checked.body;
  }
  const headers = new Headers(raw.headers);
  headers.delete('cookie');
  headers.delete('content-length');
  return deps.auth.handler(new Request(raw.url, { method: raw.method, headers, body }));
}

// Match the OAuth provider’s exact host allowlist for plain-HTTP redirects.
const LOOPBACK_HTTP = /^http:\/\/(localhost|127\.0\.0\.1|\[::1\])(:\d+)?\//;
/**
 * MCP clients on a person's machine (Claude Code, editors) register loopback
 * `http://127.0.0.1:<port>/...` redirects (RFC 8252) and often send no
 * `application_type`, which RFC 7591 defaults to `web`, where the plugin
 * requires https. A registration with no `application_type` whose redirects
 * are all loopback or private-use schemes is a native app.
 */
function nativeByDefault(m: Record<string, unknown>): Record<string, unknown> {
  const uris = m.redirect_uris;
  if (m.application_type !== undefined || !Array.isArray(uris) || uris.length === 0) return m;
  const native = uris.every((u) => typeof u === 'string' && (LOOPBACK_HTTP.test(u) || /^[a-z][a-z0-9+.-]*\.[a-z0-9+.-]+:\/?[^/]/i.test(u)));
  return native ? { ...m, application_type: 'native' } : m;
}

/**
 * Client authentication a registration may choose (review fix 5). Claude
 * registers with `client_secret_post`; it is the same secret as Basic, sent
 * in the form body. The metadata advertises exactly this list.
 */
export const AUTH_METHODS = ['none', 'client_secret_basic', 'client_secret_post'];
/**
 * Review fix 5: what a registration may ask for. Only the methods above; no
 * `jwks_uri`, `jwks` or `private_key_jwt`, so this server never fetches an
 * address a registrant chose. Then the native-by-default rule.
 */
function registration(body: string): { body: string } | { error: string } {
  let metadata: unknown;
  try {
    metadata = JSON.parse(body);
  } catch {
    return { error: 'The registration must be a JSON object.' };
  }
  if (!metadata || typeof metadata !== 'object' || Array.isArray(metadata)) return { error: 'The registration must be a JSON object.' };
  const m = metadata as Record<string, unknown>;
  const method = m.token_endpoint_auth_method;
  if (method !== undefined && !AUTH_METHODS.includes(method as string)) {
    return { error: `token_endpoint_auth_method must be one of: ${AUTH_METHODS.join(', ')}.` };
  }
  for (const key of ['jwks_uri', 'jwks', 'software_statement']) {
    if (m[key] !== undefined) return { error: `${key} is not supported.` };
  }
  return { body: JSON.stringify(nativeByDefault(m)) };
}

/** `GET /oauth2/authorize`: RFC 8707's `resource` defaults to this server's `/mcp`. */
export async function authorize(deps: GatewayDeps, raw: Request): Promise<Response> {
  const url = new URL(raw.url);
  if (!url.searchParams.has('resource')) url.searchParams.append('resource', mcpResource(deps.origin));
  // Review fix 3: consent is asked every time. A remembered consent would
  // otherwise re-authorize silently, for any loopback port a public client
  // registered, and re-grant runbooks:publish without its box being shown.
  // (`none` cannot be honoured, so it is replaced; the plugin strips
  // `consent` again once the person has answered.)
  const prompts = (url.searchParams.get('prompt') ?? '').split(' ').filter((p) => p && p !== 'none' && p !== 'consent');
  url.searchParams.set('prompt', [...prompts, 'consent'].join(' '));
  const response = await deps.auth.handler(new Request(url.toString(), { method: 'GET', headers: raw.headers }));
  // The authorize endpoint is a browser navigation: always a redirect, even
  // when Better Auth answers a fetch-like request with `{redirect, url}`.
  if (response.status === 200 && (response.headers.get('content-type') ?? '').includes('application/json')) {
    const body = (await response.clone().json().catch(() => null)) as { redirect?: boolean; url?: string } | null;
    if (body?.redirect === true && typeof body.url === 'string') {
      const headers = new Headers({ location: body.url, 'cache-control': 'no-store' });
      for (const cookie of response.headers.getSetCookie()) headers.append('set-cookie', cookie);
      return new Response(null, { status: 302, headers });
    }
  }
  return response;
}

/** The sign-in and consent pages (`/oauth/sign-in`, `/oauth/consent`). */
export async function browserPage(deps: GatewayDeps, raw: Request, path: string): Promise<Response> {
  const url = new URL(raw.url);
  if (raw.method === 'GET') {
    const query = url.search.slice(1);
    if (!query.includes('sig=')) return expired();
    if (path === '/oauth/sign-in') return deps.emailEnabled ? emailStep(query, null) : page(503, 'Sign-in unavailable', '<h1>Sign-in is not configured</h1>');
    const session = await deps.auth.api.getSession({ headers: raw.headers }).catch(() => null);
    if (!session) return see(`/api/auth/oauth/sign-in?${query}`);
    const view = await deps.auth.api.runbookConsentView({ body: { oauth_query: query } }).catch(() => null);
    return view ? consentPage(query, view, session.user.email, null) : expired();
  }
  const form = await readForm(raw);
  const query = form?.get('oauth_query') ?? '';
  if (!form || !query) return expired();
  const ip = raw.headers.get(deps.clientIpHeader);
  const call = (route: string, body: unknown, cookie?: string) =>
    deps.self(
      new Request(`${deps.origin}/api/auth${route}`, {
        method: 'POST',
        headers: { origin: deps.origin, 'content-type': 'application/json', ...(ip ? { [deps.clientIpHeader]: ip } : {}), ...(cookie ? { cookie } : {}) },
        body: JSON.stringify(body),
      }),
    );
  if (path === '/oauth/sign-in') {
    if (!deps.emailEnabled) return page(503, 'Sign-in unavailable', '<h1>Sign-in is not configured</h1>');
    const email = form.get('email') ?? '';
    const otp = form.get('otp');
    if (otp === null) {
      const sent = await call('/email-otp/send-verification-otp', { email, type: 'sign-in' });
      if (sent.status !== 200) {
        const problem = (await sent.json().catch(() => ({}))) as { message?: string };
        return emailStep(query, problem.message ?? 'We could not send a code. Try again.', sent.status);
      }
      return codeStep(query, email.trim().toLowerCase(), null);
    }
    const signedIn = await call('/sign-in/email-otp', { email, otp });
    if (signedIn.status !== 200) {
      const problem = (await signedIn.json().catch(() => ({}))) as { code?: string };
      const message = problem.code === 'auth.registration_refused' ? 'This email address has no access here. Ask an administrator to invite you.' : 'That code did not work. Check it, or start again for a new one.';
      return codeStep(query, email.trim().toLowerCase(), message, signedIn.status);
    }
    const headers = new Headers();
    for (const cookie of signedIn.headers.getSetCookie()) headers.append('set-cookie', cookie);
    return see(`/api/auth/oauth2/authorize?${authorizeQuery(query)}`, headers);
  }
  // POST /oauth/consent
  const session = await deps.auth.api.getSession({ headers: raw.headers }).catch(() => null);
  if (!session) return see(`/api/auth/oauth/sign-in?${query}`);
  const view = await deps.auth.api.runbookConsentView({ body: { oauth_query: query } }).catch(() => null);
  if (!view) return expired();
  const allow = form.get('decision') === 'allow';
  // Only scopes the page showed (the signed request's), and only those ticked:
  // never the plugin's "no scope means everything requested" default.
  const accepted = [...new Set(form.getAll('scope'))].filter((s) => view.scopes.includes(s));
  if (allow && accepted.length === 0) return consentPage(query, view, session.user.email, 'Choose at least one permission, or deny.', 400);
  const cookie = raw.headers.get('cookie') ?? '';
  const decided = await deps.auth.handler(
    new Request(`${deps.origin}/api/auth/oauth2/consent`, {
      method: 'POST',
      headers: { origin: deps.origin, accept: 'application/json', 'content-type': 'application/json', cookie, ...(ip ? { [deps.clientIpHeader]: ip } : {}) },
      body: JSON.stringify(allow ? { accept: true, scope: accepted.join(' '), oauth_query: query } : { accept: false, oauth_query: query }),
    }),
  );
  const result = (await decided.json().catch(() => null)) as { url?: string; redirect_uri?: string } | null;
  const location = result?.url ?? result?.redirect_uri;
  if (decided.status !== 200 || typeof location !== 'string') return expired();
  return see(location);
}

/** RFC 8414 metadata, at the root and at the issuer-path form. */
export function authServerMetadata(auth: OAuthAuth): (request: Request) => Promise<Response> {
  const metadata = oauthProviderAuthServerMetadata(auth as never);
  return async (request) => {
    const response = await metadata(request);
    const body = response.ok ? ((await response.clone().json().catch(() => null)) as Record<string, unknown> | null) : null;
    if (!body) return response;
    // Advertise only what registration accepts: a client picks from this list,
    // and one that picks `private_key_jwt` would then be refused.
    body.token_endpoint_auth_methods_supported = AUTH_METHODS;
    delete body.token_endpoint_auth_signing_alg_values_supported;
    const headers = new Headers(response.headers);
    headers.delete('content-length');
    return new Response(request.method === 'HEAD' ? null : JSON.stringify(body), { status: response.status, headers });
  };
}
export const METADATA_PATHS = ['/.well-known/oauth-authorization-server', '/.well-known/oauth-authorization-server/api/auth'];

/**
 * RFC 9728 protected resource metadata for the Rust `/mcp` (the host proxies
 * these paths here, so the api Worker carries no copy).
 */
export const RESOURCE_METADATA_PATHS = ['/.well-known/oauth-protected-resource', '/.well-known/oauth-protected-resource/mcp'];
export function resourceMetadata(origin: string, method: string): Response {
  const body = JSON.stringify({
    resource: mcpResource(origin),
    authorization_servers: [`${origin}/api/auth`],
    scopes_supported: SCOPE_NAMES,
    bearer_methods_supported: ['header'],
    resource_name: 'The Runbook',
  });
  return new Response(method === 'HEAD' ? null : body, {
    status: 200,
    headers: { 'content-type': 'application/json', 'cache-control': 'public, max-age=3600', 'access-control-allow-origin': '*' },
  });
}
