// The Hono app both entry points serve, and the `createAuth` factory (Shelf's
// shape). Better Auth only ever sees requests that pass the gateway below:
//   - an allowlist of Better Auth routes (everything else, every password route
//     included, is 404), filtered further by the enabled methods;
//   - the trusted-origin check on every unsafe /api/auth method (Shelf);
//   - sanitized bodies for the routes that take one;
//   - the domain restriction and the send-code limits before a code is sent.
import { betterAuth } from 'better-auth';
import { emailOTP } from 'better-auth/plugins';
import { APIError } from 'better-auth/api';
import { type GoogleProfile, verifyGoogleIdToken } from 'better-auth/social-providers';
import { Hono } from 'hono';
import { type AuthConfig, domainAllowed } from './config';
import { codeMessage, type MailTransport } from './email';
import { databaseHooks, type Fetch } from './hooks';
import { migrateCookieHeader, SESSION_COOKIE } from './cookies';
import { mountInternal, NONCE_HEADER, PROOF_HEADER, proofFor } from './internal';
import { type AuthStore, clientKey, LIMITS, sha256Hex } from './limits';
import { BROWSER_ROUTES, CLIENT_ENDPOINTS, METADATA_PATHS, RESOURCE_METADATA_PATHS, type GatewayDeps, type OAuthAuth, authServerMetadata, authorize, browserPage, clientEndpoint, oauthPlugins, resourceMetadata } from './oauth';

export const BASE_PATH = '/api/auth';
export const SESSION_TTL_SECONDS = 30 * 24 * 60 * 60;
const SESSION_UPDATE_AGE_SECONDS = 24 * 60 * 60;
const MAX_BODY_BYTES = 16 * 1024;

export interface AppDeps {
  config: AuthConfig;
  /** Better Auth's `database` option: a Drizzle adapter for D1 or Postgres. */
  database: Parameters<typeof betterAuth>[0]['database'];
  store: AuthStore;
  transport: MailTransport | null;
  /** Header that carries the client IP: `cf-connecting-ip` or `x-runbook-client-ip`. */
  clientIpHeader: string;
  /** Used for can-register; Better Auth's own Google calls use the global fetch. */
  fetch: Fetch;
}

/** The OTP identifier Better Auth's emailOTP plugin uses for sign-in codes. */
const otpIdentifier = (email: string) => `sign-in-otp-${email}`;

/**
 * Better Auth swallows errors thrown by sendVerificationOTP and answers 200, so a
 * failed delivery is recorded here against the request Better Auth hands the
 * plugin (Shelf's `delivery` object, made per request on a shared instance).
 */
const failedDeliveries = new WeakSet<Request>();

export function createAuth(deps: AppDeps) {
  const { config, transport, store } = deps;
  if (!config.origin || !config.secret) throw new Error('createAuth needs an origin and a secret');
  const google = config.google;
  return betterAuth({
    appName: 'The Runbook',
    baseURL: config.origin,
    basePath: BASE_PATH,
    secret: config.secret,
    database: deps.database,
    trustedOrigins: [config.origin],
    telemetry: { enabled: false },
    emailAndPassword: { enabled: false },
    session: { expiresIn: SESSION_TTL_SECONDS, updateAge: SESSION_UPDATE_AGE_SECONDS },
    advanced: {
      // Better Auth only knows the __Secure- prefix, so it is off and the name
      // carries __Host- itself; Secure comes from the attributes (gate fix 7).
      cookiePrefix: config.secureCookies ? '__Host-runbook' : 'runbook',
      useSecureCookies: false,
      defaultCookieAttributes: { httpOnly: true, sameSite: 'lax', path: '/', secure: config.secureCookies },
      ipAddress: { ipAddressHeaders: [deps.clientIpHeader] },
    },
    // OAuth failures land on the web app's sign-in page with ?error=<code>.
    onAPIError: { errorURL: `${config.origin}/sign-in` },
    rateLimit: { enabled: true, storage: 'database', window: 60, max: 100 },
    databaseHooks: databaseHooks(config, deps.fetch),
    socialProviders: google
      ? {
          google: {
            clientId: google.clientId,
            clientSecret: google.clientSecret,
            prompt: 'select_account',
            // Replaces Better Auth's default, which decodes the ID token without
            // checking it: verify the signature against Google's JWKS, the
            // audience and the nonce, and refuse unverified emails (§18, D45).
            async getUserInfo(token: { idToken?: string; expectedIdTokenNonce?: string }) {
              if (!token.idToken) return null;
              const claims = await verifyGoogleIdToken({ token: token.idToken, audience: google.clientId, nonce: token.expectedIdTokenNonce });
              if (!claims || claims.email_verified !== true || typeof claims.email !== 'string' || typeof claims.sub !== 'string') return null;
              return {
                user: {
                  name: typeof claims.name === 'string' ? claims.name : '',
                  email: claims.email.toLowerCase(),
                  image: typeof claims.picture === 'string' ? claims.picture : undefined,
                  emailVerified: true,
                },
                data: claims as unknown as GoogleProfile,
              };
            },
          },
        }
      : {},
    plugins: [
      // MCP2: the OAuth 2.1 authorization server for remote MCP clients.
      ...oauthPlugins(config.origin),
      ...(transport && config.email
      ? [
          emailOTP({
            otpLength: 6,
            expiresIn: 600,
            allowedAttempts: 5,
            storeOTP: 'hashed',
            rateLimit: { window: 60, max: 10 },
            async sendVerificationOTP({ email, otp, type }, ctx) {
              if (type !== 'sign-in') throw new APIError('BAD_REQUEST', { code: 'auth.invalid_request', message: 'Only sign-in codes are sent.' });
              try {
                await transport.send(codeMessage(config.email!.from, email, otp), otp);
              } catch {
                // An undelivered code must not stay usable. Never log the code or the provider error.
                await store.deleteVerification(otpIdentifier(email));
                if (ctx?.request) failedDeliveries.add(ctx.request);
              }
            },
          }),
        ]
      : []),
    ],
  });
}

export type Auth = ReturnType<typeof createAuth>;

type Json = Record<string, unknown>;
const problem = (status: number, code: string, message: string, headers: Record<string, string> = {}) =>
  new Response(JSON.stringify({ code, message }), { status, headers: { 'content-type': 'application/json', 'cache-control': 'no-store', ...headers } });

/** Better Auth routes this service exposes, by method and path below /api/auth. */
function allowedRoute(config: AuthConfig, method: string, path: string): boolean {
  const routes: string[] = [
    'GET /ok',
    'GET /get-session',
    'POST /sign-out',
    'GET /list-sessions',
    'POST /revoke-session',
    'POST /revoke-sessions',
    'POST /revoke-other-sessions',
  ];
  if (config.methods.includes('email')) routes.push('POST /email-otp/send-verification-otp', 'POST /sign-in/email-otp');
  if (config.methods.includes('google')) routes.push('POST /sign-in/social', 'GET /callback/google');
  const m = method === 'HEAD' ? 'GET' : method;
  return routes.includes(`${m} ${path}`) || CLIENT_ENDPOINTS.has(`${m} ${path}`) || BROWSER_ROUTES.has(`${m} ${path}`);
}

async function readJson(request: Request): Promise<Json | null> {
  const declared = Number(request.headers.get('content-length') ?? '0');
  if (declared > MAX_BODY_BYTES) return null;
  const text = await request.text();
  if (text.length > MAX_BODY_BYTES) return null;
  try {
    const value: unknown = JSON.parse(text);
    return value && typeof value === 'object' && !Array.isArray(value) ? (value as Json) : null;
  } catch {
    return null;
  }
}

function withBody(request: Request, body: Json): Request {
  const headers = new Headers(request.headers);
  headers.delete('content-length');
  headers.set('content-type', 'application/json');
  return new Request(request.url, { method: request.method, headers, body: JSON.stringify(body) });
}

const EMAIL = /^[^\s@]{1,64}@[^\s@]{1,253}$/;
const normalizeEmail = (value: unknown): string | null => {
  if (typeof value !== 'string') return null;
  const email = value.trim().toLowerCase();
  return email.length <= 254 && EMAIL.test(email) && email.lastIndexOf('.') > email.indexOf('@') ? email : null;
};
/** A same-origin relative path: no scheme, no `//host`, no backslashes. */
const localPath = (value: unknown) => typeof value === 'string' && /^\/(?![/\\])[^\\\s]*$/.test(value) && value.length <= 512;

export interface App {
  fetch(request: Request): Promise<Response>;
}

/**
 * Builds the app. `auth` is null when no method is configured: the service
 * still runs, reports `methods: []` and answers /api/auth with 503.
 */
export function createApp(deps: AppDeps, auth: Auth | null): App {
  const { config, transport } = deps;
  const app = new Hono();
  app.use('*', async (c, next) => {
    // Better Auth initialises lazily, and the OAuth plugin's init writes to
    // the database (it seeds the /mcp resource row). On Workers that I/O
    // belongs to the request that started it, so every request awaits it:
    // otherwise a first request that never reaches Better Auth (a 404)
    // strands it and every later request waits forever.
    if (auth) await auth.$context;
    await next();
    c.header('cache-control', 'no-store');
    c.header('x-content-type-options', 'nosniff');
    const proof = await proofFor(config.internalSecret, c.req.header(NONCE_HEADER));
    if (proof) c.header(PROOF_HEADER, proof);
  });
  mountInternal(
    app,
    config,
    auth,
    transport && config.email ? { transport, from: config.email.from } : null,
  );
  const gateway: GatewayDeps | null = auth
    ? {
        origin: config.origin!,
        emailEnabled: config.methods.includes('email'),
        clientIpHeader: deps.clientIpHeader,
        auth: auth as unknown as OAuthAuth,
        self: (request) => app.fetch(request) as Promise<Response>,
        async admitRegistration(ip) {
          const now = Date.now();
          const key = `register:${await sha256Hex(clientKey(ip))}`;
          if (!(await deps.store.claim(key, now, 0, LIMITS.registrationsPerDay, LIMITS.dayMs))) return false;
          await deps.store.sweepIdleClients(now - LIMITS.idleClientMs);
          return true;
        },
      }
    : null;

  // MCP2: RFC 8414 authorization server metadata (the issuer is {origin}/api/auth).
  for (const path of METADATA_PATHS) {
    app.on(['GET', 'HEAD'], path, (c) => (gateway ? authServerMetadata(gateway.auth)(c.req.raw) : problem(404, 'auth.not_found', 'Route not found.')));
  }
  // RFC 9728 metadata for the Rust `/mcp`, which names this server.
  for (const path of RESOURCE_METADATA_PATHS) {
    app.on(['GET', 'HEAD'], path, (c) => (gateway ? resourceMetadata(gateway.origin, c.req.method) : problem(404, 'auth.not_found', 'Route not found.')));
  }

  app.all(`${BASE_PATH}/*`, async (c) => {
    if (!auth) {
      return problem(503, 'auth.not_configured', 'No sign-in method is configured. Set RUNBOOK_SMTP_URL and RUNBOOK_SMTP_FROM for email codes, or RUNBOOK_GOOGLE_CLIENT_ID and RUNBOOK_GOOGLE_CLIENT_SECRET for Google, plus RUNBOOK_AUTH_SECRET.');
    }
    const raw = c.req.raw;
    const url = new URL(raw.url);
    const path = url.pathname.slice(BASE_PATH.length);
    if (path.endsWith('/') || path.includes('//') || path.includes('%') || !allowedRoute(config, raw.method, path)) {
      return problem(404, 'auth.not_found', 'Route not found.');
    }
    const route = `${raw.method === 'HEAD' ? 'GET' : raw.method} ${path}`;
    // MCP2: token, registration, revocation and introspection are called by
    // OAuth clients, not browsers: no Origin check, and cookies are dropped.
    if (gateway && CLIENT_ENDPOINTS.has(route)) return clientEndpoint(gateway, raw);
    const unsafe = !['GET', 'HEAD', 'OPTIONS'].includes(raw.method);
    // Shelf's trusted-origin check: browser auth mutations need our exact origin,
    // with or without a session cookie.
    if (unsafe && raw.headers.get('origin') !== config.origin) return problem(403, 'auth.csrf', 'Request origin is not allowed.');

    if (gateway && (route === 'GET /oauth2/authorize' || BROWSER_ROUTES.has(route))) {
      // Gate fix 7's legacy session cookie works on the OAuth pages too.
      const legacy = migrateCookieHeader(raw.headers.get('cookie'), config.secureCookies);
      let browser = raw;
      if (legacy) {
        const headers = new Headers(raw.headers);
        headers.set('cookie', legacy);
        browser = new Request(raw.url, { method: raw.method, headers, body: raw.method === 'GET' ? undefined : await raw.arrayBuffer() });
      }
      return route === 'GET /oauth2/authorize' ? authorize(gateway, browser) : browserPage(gateway, browser, path);
    }

    let request = raw;
    let otpEmail: string | null = null;
    if (path === '/email-otp/send-verification-otp' || path === '/sign-in/email-otp') {
      const body = await readJson(raw);
      const email = normalizeEmail(body?.email);
      otpEmail = email;
      if (!body || !email) return problem(400, 'auth.invalid_email', 'A valid email address is required.');
      if (!domainAllowed(config, email)) return problem(403, 'auth.domain_not_allowed', 'This email domain cannot sign in here.');
      if (path === '/email-otp/send-verification-otp') {
        if (body.type !== 'sign-in') return problem(400, 'auth.invalid_request', 'Only sign-in codes are sent.');
        // Per client first: the address is what an abuser varies (Shelf).
        const now = Date.now();
        // Gate fix 1: only the host's header (Rust's x-runbook-client-ip, set from
        // the peer or the client a trusted proxy names; Cloudflare's
        // cf-connecting-ip), never a client-sent X-Forwarded-For.
        const ip = clientKey(raw.headers.get(deps.clientIpHeader));
        if (!(await deps.store.claim(`ip:${await sha256Hex(ip)}`, now, 0, LIMITS.ipPerHour))) {
          return problem(429, 'auth.rate_limited', 'Too many sign-in codes requested from this address. Try again later.', { 'retry-after': '3600' });
        }
        // Gate fix 7: an address that signed in from here before has its own
        // allowance, so strangers asking for its codes cannot lock its owner out.
        const pair = await sha256Hex(`${email}\n${ip}`);
        const known = await deps.store.known(`known:${pair}`, now - LIMITS.knownForMs);
        const emailKey = known ? `email-known:${pair}` : `email:${await sha256Hex(email)}`;
        if (!(await deps.store.claim(emailKey, now, LIMITS.emailGapMs, known ? LIMITS.knownEmailPerHour : LIMITS.emailPerHour))) {
          return problem(429, 'auth.rate_limited', 'Please wait a minute before requesting another code.', { 'retry-after': '60' });
        }
        request = withBody(raw, { email, type: 'sign-in' });
      } else {
        if (typeof body.otp !== 'string' || !/^\d{6}$/.test(body.otp)) return problem(400, 'auth.invalid_code', 'Enter the 6-digit code from the email.');
        // Names, images and any other fields stay out: the Rust directory owns profiles.
        request = withBody(raw, { email, otp: body.otp });
      }
    } else if (path === '/sign-in/social') {
      const body = await readJson(raw);
      if (!body || body.provider !== 'google' || 'idToken' in body) return problem(400, 'auth.invalid_request', 'Only Google sign-in by redirect is supported.');
      const forwarded: Json = { provider: 'google', callbackURL: body.callbackURL ?? '/' };
      for (const key of ['callbackURL', 'errorCallbackURL', 'newUserCallbackURL'] as const) {
        if (body[key] === undefined) continue;
        if (!localPath(body[key])) return problem(400, 'auth.invalid_callback', `${key} must be a path on this origin.`);
        forwarded[key] = body[key];
      }
      request = withBody(raw, forwarded);
    } else if (unsafe) {
      // Session routes (sign-out, revoke-*): pass through with a bounded JSON body, if any.
      const text = await raw.text();
      let body: unknown = {};
      try {
        body = text.trim() === '' ? {} : JSON.parse(text);
      } catch {
        body = null;
      }
      if (text.length > MAX_BODY_BYTES || !body || typeof body !== 'object' || Array.isArray(body)) {
        return problem(400, 'auth.invalid_request', 'Expected a small JSON body.');
      }
      request = withBody(raw, body as Json);
    }
    // Gate fix 7: a pre-__Host- session cookie still signs in for one release.
    const migrated = migrateCookieHeader(request.headers.get('cookie'), config.secureCookies);
    if (migrated) {
      const headers = new Headers(request.headers);
      headers.set('cookie', migrated);
      const body = ['GET', 'HEAD'].includes(request.method) ? undefined : await request.arrayBuffer();
      request = new Request(request.url, { method: request.method, headers, body });
    }
    let response = await auth.handler(request);
    if (failedDeliveries.has(request)) {
      return problem(503, 'auth.email_unavailable', 'We could not send your code. Please try again in a minute.');
    }
    if (path === '/sign-in/email-otp' && response.status === 200 && otpEmail) {
      // Gate fix 7: this client address has proved this mailbox.
      await deps.store.markKnown(`known:${await sha256Hex(`${otpEmail}\n${clientKey(raw.headers.get(deps.clientIpHeader))}`)}`, Date.now());
    }
    if (migrated && response.headers.getSetCookie().some((c) => c.startsWith(`${SESSION_COOKIE.https}=`))) {
      // The new name is set (sign-in, renewal or sign-out): retire the old one.
      response = new Response(response.body, response);
      response.headers.append('set-cookie', `${SESSION_COOKIE.legacy}=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Lax`);
    }
    return response;
  });

  app.all('*', () => problem(404, 'auth.not_found', 'Route not found.'));
  return { fetch: (request: Request) => app.fetch(request) as Promise<Response> };
}
