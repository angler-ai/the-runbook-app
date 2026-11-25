// MCP2: the OAuth 2.1 authorization server, run on Miniflare/D1 and on
// Node/Postgres with the same harness `h` as ./suite.mjs, plus:
//   expireAccessTokens()          -> moves every OAuth access token's expiry into the past
//   backdateClient(clientId, ms)  -> sets a registered client's created_at
//   clientExists(clientId)        -> whether the client row is still there
//   backdateConsent(clientId, ms) -> sets a client's consents' created_at and updated_at
//   breakTokens() / fixTokens()   -> hides and restores the access token table (a backend failure)
//   deleteClient(clientId), deleteUser(email)
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { INTERNAL_SECRET, ORIGIN } from './stubs.mjs';

const RESOURCE = `${ORIGIN}/mcp`;
const ISSUER = `${ORIGIN}/api/auth`;
const REDIRECT = 'http://127.0.0.1:53682/callback';
let ipCounter = 0;
const freshIp = () => {
  const n = ++ipCounter;
  return `198.18.${(n >> 8) & 255}.${n & 255}`;
};

/** `h.request` with a client address of its own unless one is given (Better Auth limits per address). */
const req = (h, path, init = {}) => h.request(path, { ...init, headers: { [h.ipHeader]: freshIp(), ...(init.headers ?? {}) } });

const b64url = (buf) => Buffer.from(buf).toString('base64url');
export function pkce() {
  const verifier = b64url(randomBytes(32));
  return { verifier, challenge: b64url(createHash('sha256').update(verifier).digest()) };
}

const form = (h, path, fields, { origin = ORIGIN, cookie, ip = freshIp() } = {}) =>
  req(h, path, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded', ...(origin ? { origin } : {}), ...(cookie ? { cookie } : {}), [h.ipHeader]: ip },
    body: new URLSearchParams(fields).toString(),
  });

/** Cookie jar: name=value pairs from Set-Cookie, merged. */
function jar(cookie, response) {
  const map = new Map((cookie ? cookie.split('; ') : []).map((p) => p.split(/=(.*)/s).slice(0, 2)));
  for (const c of response.headers.getSetCookie()) {
    const [pair] = c.split(';');
    const [name, value] = pair.split(/=(.*)/s);
    if (value === '' || /max-age=0/i.test(c)) map.delete(name);
    else map.set(name, value);
  }
  return [...map].map(([k, v]) => `${k}=${v}`).join('; ');
}

const pathOf = (location) => {
  const url = new URL(location, ORIGIN);
  return `${url.pathname}${url.search}`;
};

export async function register(h, metadata = {}) {
  const r = await req(h, '/api/auth/oauth2/register', {
    method: 'POST',
    headers: { 'content-type': 'application/json', [h.ipHeader]: freshIp() },
    body: JSON.stringify({ client_name: 'Test MCP client', redirect_uris: [REDIRECT], token_endpoint_auth_method: 'none', grant_types: ['authorization_code', 'refresh_token'], response_types: ['code'], ...metadata }),
  });
  const body = await r.json();
  assert.equal(r.status, 201, JSON.stringify(body));
  return body;
}

export function authorizeUrl(client, { challenge, scope, resource = RESOURCE, redirect = REDIRECT, method = 'S256', state = 'st-1', extra = {} } = {}) {
  const params = new URLSearchParams({ response_type: 'code', client_id: client.client_id, redirect_uri: redirect, state, ...extra });
  if (challenge) params.set('code_challenge', challenge);
  if (challenge && method) params.set('code_challenge_method', method);
  if (scope) params.set('scope', scope);
  if (resource) params.set('resource', resource);
  return `/api/auth/oauth2/authorize?${params}`;
}

/** Signs in through the OAuth sign-in page; returns the cookie and the authorize location it continues to. */
async function signInPage(h, location, email) {
  const page = await req(h, pathOf(location));
  assert.equal(page.status, 200);
  assert.match(page.headers.get('content-type'), /^text\/html/);
  assert.match(page.headers.get('content-security-policy'), /default-src 'none'/);
  const query = new URL(location, ORIGIN).search.slice(1);
  const ip = freshIp();
  const sent = await form(h, '/api/auth/oauth/sign-in', { oauth_query: query, email }, { ip });
  assert.equal(sent.status, 200, await sent.clone().text());
  assert.match(await sent.text(), /6-digit code/);
  const code = h.mail().filter((m) => m.to === email).at(-1)?.code;
  assert.match(code, /^\d{6}$/);
  const done = await form(h, '/api/auth/oauth/sign-in', { oauth_query: query, email, otp: code }, { ip });
  assert.equal(done.status, 303, await done.clone().text());
  const next = done.headers.get('location');
  assert.match(next, /^\/api\/auth\/oauth2\/authorize\?/);
  assert.ok(!/[?&](sig|exp|ba_iat|ba_param)=/.test(next), 'the signing params are dropped');
  return { cookie: jar('', done), next };
}

/**
 * Runs register → authorize (PKCE) → sign-in → consent → code. `tick` lists the
 * scopes the person leaves ticked; null ticks what the page ticked by default.
 */
export async function consentFlow(h, { email, scope, tick = null, client, pk = pkce(), cookie = '' } = {}) {
  client ??= await register(h);
  let r = await req(h, authorizeUrl(client, { challenge: pk.challenge, scope }), { headers: cookie ? { cookie } : {} });
  assert.equal(r.status, 302, await r.clone().text());
  let location = r.headers.get('location');
  if (new URL(location, ORIGIN).pathname === '/api/auth/oauth/sign-in') {
    const signedIn = await signInPage(h, location, email);
    cookie = signedIn.cookie;
    r = await req(h, signedIn.next, { headers: { cookie } });
    assert.equal(r.status, 302, await r.clone().text());
    location = r.headers.get('location');
  }
  if (location.startsWith(`${REDIRECT}?`)) {
    // Consent given before for these scopes: straight back with a code.
    const back = new URL(location);
    return { client, pk, cookie, code: back.searchParams.get('code'), html: null, shown: null, query: null };
  }
  assert.equal(new URL(location, ORIGIN).pathname, '/api/auth/oauth/consent', location);
  const consent = await req(h, pathOf(location), { headers: { cookie } });
  assert.equal(consent.status, 200);
  const html = await consent.text();
  const query = new URL(location, ORIGIN).search.slice(1);
  const defaults = [...html.matchAll(/<input type="checkbox" name="scope" value="([^"]+)"( checked)?>/g)];
  const shown = defaults.map((m) => m[1]);
  const ticked = tick ?? defaults.filter((m) => m[2]).map((m) => m[1]);
  const fields = new URLSearchParams({ oauth_query: query, decision: 'allow' });
  for (const s of ticked) fields.append('scope', s);
  const decided = await req(h, '/api/auth/oauth/consent', {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded', origin: ORIGIN, cookie, [h.ipHeader]: freshIp() },
    body: fields.toString(),
  });
  assert.equal(decided.status, 303, await decided.clone().text());
  const back = new URL(decided.headers.get('location'));
  assert.equal(`${back.origin}${back.pathname}`, REDIRECT);
  assert.equal(back.searchParams.get('state'), 'st-1');
  assert.equal(back.searchParams.get('iss'), ISSUER);
  return { client, pk, cookie, code: back.searchParams.get('code'), html, shown, query };
}

export const tokenRequest = (h, fields) =>
  req(h, '/api/auth/oauth2/token', {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded', [h.ipHeader]: freshIp() },
    body: new URLSearchParams(fields).toString(),
  });

export async function exchange(h, { client, code, pk, redirect = REDIRECT, resource = RESOURCE }) {
  return tokenRequest(h, { grant_type: 'authorization_code', client_id: client.client_id, code, code_verifier: pk.verifier, redirect_uri: redirect, ...(resource ? { resource } : {}) });
}

/** Rust's introspection: 200 with the session shape when active for `resource`, else 401. */
export const introspect = async (h, token, { secret = INTERNAL_SECRET, resource = RESOURCE } = {}) => {
  const r = await req(h, '/internal/oauth/introspect', { method: 'POST', headers: { 'content-type': 'application/json', ...(secret ? { 'x-runbook-internal-secret': secret } : {}) }, body: JSON.stringify({ token, resource }) });
  const body = await r.json();
  return { status: r.status, body, active: r.status === 200 };
};

export function oauthSuite(h) {
  test('OAuth: protected resource metadata for /mcp (RFC 9728)', async () => {
    for (const path of ['/.well-known/oauth-protected-resource', '/.well-known/oauth-protected-resource/mcp']) {
      const r = await req(h, path);
      assert.equal(r.status, 200, path);
      assert.deepEqual(await r.json(), {
        resource: RESOURCE,
        authorization_servers: [ISSUER],
        scopes_supported: ['runbooks:read', 'runbooks:write', 'runbooks:publish', 'executions:read', 'executions:start', 'executions:act', 'skills:read', 'runner:read', 'runner:admin', 'offline_access'],
        bearer_methods_supported: ['header'],
        resource_name: 'The Runbook',
      });
    }
  });

  test('OAuth: authorization server metadata (RFC 8414)', async () => {
    for (const path of ['/.well-known/oauth-authorization-server', '/.well-known/oauth-authorization-server/api/auth']) {
      const r = await req(h, path);
      assert.equal(r.status, 200, path);
      const m = await r.json();
      assert.equal(m.issuer, ISSUER);
      assert.equal(m.authorization_endpoint, `${ISSUER}/oauth2/authorize`);
      assert.equal(m.token_endpoint, `${ISSUER}/oauth2/token`);
      assert.equal(m.registration_endpoint, `${ISSUER}/oauth2/register`);
      assert.deepEqual(m.code_challenge_methods_supported, ['S256']);
      assert.deepEqual(m.response_types_supported, ['code']);
      assert.deepEqual([...m.grant_types_supported].sort(), ['authorization_code', 'refresh_token']);
      // Exactly what registration accepts (a client picks one of these).
      assert.deepEqual(m.token_endpoint_auth_methods_supported, ['none', 'client_secret_basic', 'client_secret_post']);
      assert.equal(m.token_endpoint_auth_signing_alg_values_supported, undefined);
      assert.ok(m.scopes_supported.includes('runbooks:publish') && m.scopes_supported.includes('offline_access'));
      for (const s of ['tokens:manage', 'project:admin', 'webhooks:manage', 'openid']) assert.ok(!m.scopes_supported.includes(s), s);
    }
  });

  test('OAuth: the full flow — register, PKCE, sign-in, consent, token, introspect, refresh, revoke', async () => {
    const email = 'oauth-ada@example.test';
    h.canRegister.state.allowed.add(email);
    const flow = await consentFlow(h, { email, scope: 'runbooks:read executions:read executions:act offline_access' });
    assert.ok(flow.code);
    assert.deepEqual(flow.shown, ['runbooks:read', 'executions:read', 'executions:act', 'offline_access']);
    const r = await exchange(h, flow);
    const tokens = await r.json();
    assert.equal(r.status, 200, JSON.stringify(tokens));
    assert.equal(tokens.token_type, 'Bearer');
    assert.match(tokens.access_token, /^trb_oat_[A-Za-z]{32}$/);
    assert.match(tokens.refresh_token, /^trb_ort_/);
    assert.equal(tokens.scope.split(' ').sort().join(' '), 'executions:act executions:read offline_access runbooks:read');
    assert.equal(tokens.expires_in, 3600);
    const seen = await introspect(h, tokens.access_token);
    assert.equal(seen.status, 200);
    assert.deepEqual(Object.keys(seen.body).sort(), ['auth_user_id', 'client_id', 'email', 'email_verified', 'expires_at', 'mfa_enrolled', 'mfa_method', 'mfa_verified_at', 'scope', 'session_id']);
    assert.equal(seen.body.email, email);
    assert.equal(seen.body.email_verified, true);
    assert.equal(seen.body.client_id, flow.client.client_id);
    assert.equal(seen.body.scope.split(' ').sort().join(' '), 'executions:act executions:read offline_access runbooks:read');
    assert.match(seen.body.session_id, /^oauth:[0-9a-f]{32}$/);
    assert.ok(seen.body.expires_at > Date.now() + 3500_000 && seen.body.expires_at <= Date.now() + 3600_000);
    assert.equal(typeof seen.body.auth_user_id, 'string');
    // The same token, asked for by another resource: not active there.
    assert.equal((await introspect(h, tokens.access_token, { resource: 'https://elsewhere.example.test/mcp' })).status, 401);
    // Refresh rotates: a new pair, and the old refresh token is spent.
    const refreshed = await tokenRequest(h, { grant_type: 'refresh_token', client_id: flow.client.client_id, refresh_token: tokens.refresh_token, resource: RESOURCE });
    const next = await refreshed.json();
    assert.equal(refreshed.status, 200, JSON.stringify(next));
    assert.notEqual(next.access_token, tokens.access_token);
    assert.notEqual(next.refresh_token, tokens.refresh_token);
    assert.equal((await introspect(h, next.access_token)).active, true);
    // Revocation ends the access token.
    const revoked = await req(h, '/api/auth/oauth2/revoke', {
      method: 'POST',
      headers: { 'content-type': 'application/x-www-form-urlencoded', [h.ipHeader]: freshIp() },
      body: new URLSearchParams({ token: next.access_token, token_type_hint: 'access_token', client_id: flow.client.client_id }).toString(),
    });
    assert.equal(revoked.status, 200, await revoked.clone().text());
    assert.equal((await introspect(h, next.access_token)).active, false);
    // Reusing a spent refresh token is refused and revokes the family (RFC 9700).
    const reused = await tokenRequest(h, { grant_type: 'refresh_token', client_id: flow.client.client_id, refresh_token: tokens.refresh_token });
    assert.equal(reused.status, 400, 'a spent refresh token is refused');
    const afterReuse = await tokenRequest(h, { grant_type: 'refresh_token', client_id: flow.client.client_id, refresh_token: next.refresh_token });
    assert.equal(afterReuse.status, 400, 'the family is revoked');
    // A code is single use, and replaying it revokes what it issued (RFC 9700).
    const again = await consentFlow(h, { email, client: flow.client, cookie: flow.cookie, scope: 'runbooks:read' });
    const issued = await (await exchange(h, again)).json();
    assert.equal((await introspect(h, issued.access_token)).active, true);
    assert.equal((await exchange(h, again)).status, 400);
    assert.equal((await introspect(h, issued.access_token)).active, false);
  });

  test('OAuth: runbooks:publish is granted only when the consent shows it and it is ticked', async () => {
    const email = 'oauth-pub@example.test';
    h.canRegister.state.allowed.add(email);
    const scope = 'runbooks:read runbooks:write runbooks:publish';
    const untouched = await consentFlow(h, { email, scope });
    assert.match(untouched.html, /<input type="checkbox" name="scope" value="runbooks:publish"> /, 'shown, unticked');
    assert.match(untouched.html, /not granted unless you tick it/);
    const t1 = await (await exchange(h, untouched)).json();
    assert.equal(t1.scope.split(' ').sort().join(' '), 'runbooks:read runbooks:write');
    assert.ok(!(await introspect(h, t1.access_token)).body.scope.includes('publish'));
    assert.ok((await introspect(h, (await (await exchange(h, await consentFlow(h, { email, scope, client: untouched.client, cookie: untouched.cookie, tick: ['runbooks:read'] }))).json()).access_token)).active);
    // Ticked: granted. A new consent is asked, since the last one lacked it.
    const ticked = await consentFlow(h, { email, scope, client: untouched.client, cookie: untouched.cookie, tick: ['runbooks:read', 'runbooks:write', 'runbooks:publish'] });
    const t2 = await (await exchange(h, ticked)).json();
    assert.ok(t2.scope.split(' ').includes('runbooks:publish'));
    // No scope parameter: the client's registered scopes are requested, and the
    // page still shows publish unticked.
    const defaulted = await consentFlow(h, { email, client: await register(h), cookie: untouched.cookie });
    assert.ok(defaulted.shown.includes('runbooks:publish'));
    const t3 = await (await exchange(h, defaulted)).json();
    assert.ok(!t3.scope.split(' ').includes('runbooks:publish'), t3.scope);
  });

  test('OAuth: the pages keep same-origin referrers, refuse framing, and never accept Origin: null', async () => {
    // Review fix 1: a `no-referrer` page makes browsers post `Origin: null`.
    const client = await register(h);
    const r = await req(h, authorizeUrl(client, { challenge: pkce().challenge, scope: 'runbooks:read' }));
    const signIn = await req(h, pathOf(r.headers.get('location')));
    assert.equal(signIn.status, 200);
    assert.equal(signIn.headers.get('referrer-policy'), 'same-origin');
    assert.equal(signIn.headers.get('x-frame-options'), 'DENY');
    assert.match(signIn.headers.get('content-security-policy'), /frame-ancestors 'none'/);
    assert.doesNotMatch(await signIn.text(), /name="referrer"/);
    const query = new URL(r.headers.get('location'), ORIGIN).search.slice(1);
    for (const origin of ['null', null]) {
      const posted = await form(h, '/api/auth/oauth/sign-in', { oauth_query: query, email: 'oauth-null@example.test' }, { origin });
      assert.equal(posted.status, 403, String(origin));
    }
  });

  test('OAuth: consent refuses scopes it did not show, empty grants, and cross-site posts', async () => {
    const email = 'oauth-consent@example.test';
    h.canRegister.state.allowed.add(email);
    const client = await register(h);
    const pk = pkce();
    let r = await req(h, authorizeUrl(client, { challenge: pk.challenge, scope: 'runbooks:read' }));
    const signedIn = await signInPage(h, r.headers.get('location'), email);
    r = await req(h, signedIn.next, { headers: { cookie: signedIn.cookie } });
    const location = r.headers.get('location');
    const query = new URL(location, ORIGIN).search.slice(1);
    const post = (fields, opts = {}) => form(h, '/api/auth/oauth/consent', { oauth_query: query, ...fields }, { cookie: signedIn.cookie, ...opts });
    // A scope that was never requested is dropped, so nothing is left to grant.
    const sneaky = await post({ decision: 'allow', scope: 'runbooks:publish' });
    assert.equal(sneaky.status, 400);
    assert.match(await sneaky.text(), /Choose at least one permission/);
    // Cross-site form post: refused before anything happens.
    const csrf = await post({ decision: 'allow', scope: 'runbooks:read' }, { origin: 'https://evil.example.test' });
    assert.equal(csrf.status, 403);
    // A tampered query fails its signature.
    const tampered = await form(h, '/api/auth/oauth/consent', { oauth_query: query.replace('runbooks%3Aread', 'runbooks%3Apublish'), decision: 'allow', scope: 'runbooks:publish' }, { cookie: signedIn.cookie });
    assert.equal(tampered.status, 400);
    assert.match(await (await req(h, `/api/auth/oauth/consent?${query.replace('runbooks%3Aread', 'runbooks%3Apublish')}`, { headers: { cookie: signedIn.cookie } })).text(), /expired/);
    // Deny: back to the client with access_denied.
    const denied = await post({ decision: 'deny' });
    assert.equal(denied.status, 303);
    const back = new URL(denied.headers.get('location'));
    assert.equal(back.searchParams.get('error'), 'access_denied');
    assert.equal(back.searchParams.get('code'), null);
  });

  test('OAuth: PKCE is required, S256 only, and the verifier must match', async () => {
    const email = 'oauth-pkce@example.test';
    h.canRegister.state.allowed.add(email);
    const client = await register(h);
    const errorOf = async (url) => {
      const r = await req(h, url);
      assert.equal(r.status, 302);
      const to = new URL(r.headers.get('location'));
      assert.equal(`${to.origin}${to.pathname}`, REDIRECT);
      return to.searchParams.get('error');
    };
    assert.equal(await errorOf(authorizeUrl(client, { scope: 'runbooks:read' })), 'invalid_request', 'no code_challenge');
    assert.equal(await errorOf(authorizeUrl(client, { challenge: pkce().verifier, method: 'plain', scope: 'runbooks:read' })), 'invalid_request', 'plain');
    const flow = await consentFlow(h, { email, client, scope: 'runbooks:read' });
    const wrong = await exchange(h, { ...flow, pk: pkce() });
    assert.ok([400, 401].includes(wrong.status), `wrong verifier: ${wrong.status}`);
    const refusal = await wrong.json();
    assert.ok(['invalid_grant', 'invalid_client', 'invalid_request'].includes(refusal.error), JSON.stringify(refusal));
    assert.equal(refusal.access_token, undefined);
    const none = await tokenRequest(h, { grant_type: 'authorization_code', client_id: client.client_id, code: flow.code, redirect_uri: REDIRECT });
    assert.ok([400, 401].includes(none.status), `no verifier: ${none.status}`);
    assert.equal((await none.json()).access_token, undefined);
  });

  test('OAuth: a client_secret_post client (how Claude registers) gets tokens only with its secret in the body', async () => {
    const email = 'oauth-post@example.test';
    h.canRegister.state.allowed.add(email);
    const client = await register(h, { client_name: 'Claude', token_endpoint_auth_method: 'client_secret_post' });
    assert.equal(client.token_endpoint_auth_method, 'client_secret_post');
    assert.ok(client.client_secret);
    const fields = (flow) => ({ grant_type: 'authorization_code', client_id: client.client_id, code: flow.code, code_verifier: flow.pk.verifier, redirect_uri: REDIRECT, resource: RESOURCE });
    const missing = await consentFlow(h, { email, scope: 'runbooks:read', client });
    const refused = await tokenRequest(h, fields(missing));
    assert.ok([400, 401].includes(refused.status), `no secret: ${refused.status}`);
    assert.equal((await refused.json()).access_token, undefined);
    const flow = await consentFlow(h, { email, scope: 'runbooks:read', client, cookie: missing.cookie });
    const r = await tokenRequest(h, { ...fields(flow), client_secret: client.client_secret });
    const tokens = await r.json();
    assert.equal(r.status, 200, JSON.stringify(tokens));
    assert.equal((await introspect(h, tokens.access_token)).active, true);
  });

  test('OAuth: redirect URIs must be registered, exactly', async () => {
    const email = 'oauth-redirect@example.test';
    h.canRegister.state.allowed.add(email);
    const client = await register(h);
    const pk = pkce();
    const r = await req(h, authorizeUrl(client, { challenge: pk.challenge, scope: 'runbooks:read', redirect: 'https://evil.example.test/callback' }));
    assert.equal(r.status, 302);
    assert.ok(!r.headers.get('location').startsWith('https://evil.example.test'), 'never sent to an unregistered URI');
    const flow = await consentFlow(h, { email, client, scope: 'runbooks:read', pk });
    const other = await exchange(h, { ...flow, redirect: 'http://127.0.0.1:53682/other' });
    assert.equal(other.status, 400);
  });

  test('OAuth: tokens name this /mcp, and another resource is refused', async () => {
    const email = 'oauth-resource@example.test';
    h.canRegister.state.allowed.add(email);
    const client = await register(h);
    const r = await req(h, authorizeUrl(client, { challenge: pkce().challenge, scope: 'runbooks:read', resource: 'https://elsewhere.example.test/mcp' }));
    assert.equal(r.status, 302);
    assert.equal(new URL(r.headers.get('location')).searchParams.get('error'), 'invalid_target');
    // No resource parameter: the token is still bound to this /mcp.
    const pk = pkce();
    const start = await req(h, authorizeUrl(client, { challenge: pk.challenge, scope: 'runbooks:read', resource: null }));
    const signedIn = await signInPage(h, start.headers.get('location'), email);
    assert.ok(new URLSearchParams(signedIn.next.split('?')[1]).getAll('resource').includes(RESOURCE));
    const flow = await consentFlow(h, { email, client, scope: 'runbooks:read', pk, cookie: signedIn.cookie });
    const tokens = await (await exchange(h, { ...flow, resource: null })).json();
    assert.equal((await introspect(h, tokens.access_token)).active, true);
    assert.equal((await introspect(h, tokens.access_token, { resource: 'https://elsewhere.example.test/mcp' })).active, false);
    // A token request for another resource fails.
    const flow2 = await consentFlow(h, { email, client, scope: 'runbooks:read', cookie: signedIn.cookie });
    const other = await exchange(h, { ...flow2, resource: 'https://elsewhere.example.test/mcp' });
    assert.equal(other.status, 400);
  });

  test('OAuth: expired and unknown tokens are inactive; introspection needs the secret', async () => {
    const email = 'oauth-expiry@example.test';
    h.canRegister.state.allowed.add(email);
    const flow = await consentFlow(h, { email, scope: 'runbooks:read offline_access' });
    const tokens = await (await exchange(h, flow)).json();
    assert.match(tokens.refresh_token, /^trb_ort_/);
    assert.equal((await introspect(h, tokens.access_token)).active, true);
    const noSecret = await introspect(h, tokens.access_token, { secret: null });
    assert.equal(noSecret.body.code, 'auth.internal_unauthorized');
    assert.equal((await introspect(h, tokens.access_token, { secret: `${INTERNAL_SECRET}x` })).body.code, 'auth.internal_unauthorized');
    for (const token of ['trb_oat_unknownunknownunknownunknownxx', tokens.refresh_token, 'trb_pat_x']) {
      const r = await introspect(h, token);
      assert.equal(r.status, 401, token.slice(0, 8));
      assert.equal(r.body.code, 'auth.no_session');
    }
    await h.expireAccessTokens();
    assert.equal((await introspect(h, tokens.access_token)).active, false);
  });

  test('OAuth: client endpoints ignore cookies and need no Origin; registration is bounded', async () => {
    const email = 'oauth-cookie@example.test';
    h.canRegister.state.allowed.add(email);
    const flow = await consentFlow(h, { email, scope: 'runbooks:read' });
    // A browser session cookie does not make a registration session-backed.
    const r = await req(h, '/api/auth/oauth2/register', {
      method: 'POST',
      headers: { 'content-type': 'application/json', cookie: flow.cookie, origin: 'https://evil.example.test', [h.ipHeader]: freshIp() },
      body: JSON.stringify({ client_name: 'x', redirect_uris: [REDIRECT], token_endpoint_auth_method: 'none' }),
    });
    assert.equal(r.status, 201);
    assert.equal((await r.json()).user_id, undefined);
    const cc = await req(h, '/api/auth/oauth2/register', {
      method: 'POST',
      headers: { 'content-type': 'application/json', [h.ipHeader]: freshIp() },
      body: JSON.stringify({ client_name: 'm2m', redirect_uris: [REDIRECT], grant_types: ['client_credentials'] }),
    });
    assert.equal(cc.status, 400, 'no client_credentials grant');
    // Other plugin routes stay closed.
    for (const [method, path] of [['GET', '/api/auth/oauth2/get-clients'], ['POST', '/api/auth/oauth2/create-client'], ['POST', '/api/auth/oauth2/consent'], ['GET', '/api/auth/oauth2/userinfo'], ['POST', '/api/auth/admin/oauth2/create-client']]) {
      const closed = await req(h, path, { method, headers: { origin: ORIGIN, cookie: flow.cookie, 'content-type': 'application/json' }, ...(method === 'POST' ? { body: '{}' } : {}) });
      assert.equal(closed.status, 404, `${method} ${path}`);
    }
  });

  test('OAuth: the consent page names the redirect host and an unverified, sanitized client name (review 2)', async () => {
    const email = 'oauth-phish@example.test';
    h.canRegister.state.allowed.add(email);
    const long = 'Very trustworthy app '.repeat(10);
    const client = await register(h, {
      client_name: `<script>alert(1)</script>‮exe.gnp​\u0007 ${long}`,
      redirect_uris: ['https://app.example.test/oauth/callback'],
    });
    const start = await req(h, authorizeUrl(client, { challenge: pkce().challenge, scope: 'runbooks:read', redirect: 'https://app.example.test/oauth/callback' }));
    const signedIn = await signInPage(h, start.headers.get('location'), email);
    const r = await req(h, signedIn.next, { headers: { cookie: signedIn.cookie } });
    const page = await req(h, pathOf(r.headers.get('location')), { headers: { cookie: signedIn.cookie } });
    assert.equal(page.headers.get('x-frame-options'), 'DENY');
    assert.match(page.headers.get('content-security-policy'), /frame-ancestors 'none'/);
    const html = await page.text();
    assert.doesNotMatch(html, /<script>/);
    assert.match(html, /&#60;script&#62;alert\(1\)&#60;\/script&#62;exe\.gnp /);
    for (const c of ['‮', '​', '\u0007']) assert.ok(!html.includes(c), `stripped U+${c.codePointAt(0).toString(16)}`);
    const heading = /<h1>Allow (.*) to use The Runbook\?<\/h1>/.exec(html)[1].replace(/&#\d+;/g, 'x');
    assert.ok(heading.length <= 64 && heading.endsWith('…'), heading);
    assert.match(html, /unverified \(self-registered\)/);
    assert.match(html, /you are sent back to <strong class="host">app\.example\.test<\/strong>/);
    // A loopback client says it is an app on this computer.
    const local = await consentFlow(h, { email, scope: 'runbooks:read', cookie: signedIn.cookie });
    assert.match(local.html, /<strong class="host">127\.0\.0\.1:53682 \(an app on this computer\)<\/strong>/);
  });

  test('OAuth: consent is asked every time, so a remembered consent never re-grants silently (review 3)', async () => {
    const email = 'oauth-again@example.test';
    h.canRegister.state.allowed.add(email);
    const first = await consentFlow(h, { email, scope: 'runbooks:read runbooks:publish', tick: ['runbooks:read', 'runbooks:publish'] });
    assert.ok((await (await exchange(h, first)).json()).scope.includes('runbooks:publish'));
    // The same public client, the same person, publish again: the page again,
    // with publish unticked.
    const again = await req(h, authorizeUrl(first.client, { challenge: pkce().challenge, scope: 'runbooks:read runbooks:publish' }), { headers: { cookie: first.cookie } });
    assert.equal(again.status, 302);
    assert.equal(new URL(again.headers.get('location'), ORIGIN).pathname, '/api/auth/oauth/consent');
    const page = await (await req(h, pathOf(again.headers.get('location')), { headers: { cookie: first.cookie } })).text();
    assert.match(page, /<input type="checkbox" name="scope" value="runbooks:publish"> /);
    // Without publish too, and prompt=none cannot skip it.
    for (const extra of [{}, { prompt: 'none' }]) {
      const r = await req(h, authorizeUrl(first.client, { challenge: pkce().challenge, scope: 'runbooks:read', extra }), { headers: { cookie: first.cookie } });
      assert.equal(new URL(r.headers.get('location'), ORIGIN).pathname, '/api/auth/oauth/consent', JSON.stringify(extra));
    }
  });

  test('OAuth: a grant outlives the web session that consented to it (review 4)', async () => {
    const email = 'oauth-outlive@example.test';
    h.canRegister.state.allowed.add(email);
    const flow = await consentFlow(h, { email, scope: 'runbooks:read offline_access' });
    const tokens = await (await exchange(h, flow)).json();
    const session = await (await req(h, '/internal/session', { method: 'POST', headers: { 'x-runbook-internal-secret': INTERNAL_SECRET, cookie: flow.cookie } })).json();
    // The session expires, then the person signs out of the web app.
    await h.setSessionExpiry(session.session_id, Date.now() - 1000);
    assert.equal((await introspect(h, tokens.access_token)).active, true, 'an expired web session leaves the token');
    // Signing out of the web app ends the access tokens minted under that
    // session (the plugin revokes them), but not the grant: the
    // offline_access refresh token stays, and the tokens it mints work.
    const out = await req(h, '/api/auth/sign-out', { method: 'POST', headers: { origin: ORIGIN, cookie: flow.cookie, 'content-type': 'application/json', [h.ipHeader]: freshIp() }, body: '{}' });
    assert.equal(out.status, 200, await out.clone().text());
    assert.equal((await introspect(h, tokens.access_token)).active, false, 'sign-out ends the session-minted access token');
    // Stay connected: refresh, and the new token works.
    const refreshed = await tokenRequest(h, { grant_type: 'refresh_token', client_id: flow.client.client_id, refresh_token: tokens.refresh_token });
    const next = await refreshed.json();
    assert.equal(refreshed.status, 200, JSON.stringify(next));
    assert.equal((await introspect(h, next.access_token)).active, true);
    // A grant without offline_access ends with the session's sign-out.
    const other = 'oauth-outlive-2@example.test';
    h.canRegister.state.allowed.add(other);
    const short = await consentFlow(h, { email: other, scope: 'runbooks:read' });
    const shortTokens = await (await exchange(h, short)).json();
    assert.equal(shortTokens.refresh_token, undefined);
    await req(h, '/api/auth/sign-out', { method: 'POST', headers: { origin: ORIGIN, cookie: short.cookie, 'content-type': 'application/json', [h.ipHeader]: freshIp() }, body: '{}' });
    assert.equal((await introspect(h, shortTokens.access_token)).active, false);
  });

  test('OAuth: registration takes only none, client_secret_basic and client_secret_post, no keys to fetch, a daily cap per address, and idle clients are swept (review 5)', async () => {
    const post = (metadata, ip = freshIp()) =>
      req(h, '/api/auth/oauth2/register', {
        method: 'POST',
        headers: { 'content-type': 'application/json', [h.ipHeader]: ip },
        body: JSON.stringify({ client_name: 'r', redirect_uris: [REDIRECT], ...metadata }),
      });
    for (const bad of [
      { token_endpoint_auth_method: 'private_key_jwt' },
      { token_endpoint_auth_method: 'client_secret_jwt' },
      { jwks_uri: 'https://keys.example.test/jwks.json' },
      { jwks: { keys: [] } },
    ]) {
      const r = await post(bad);
      assert.equal(r.status, 400, JSON.stringify(bad));
      assert.equal((await r.json()).error, 'invalid_client_metadata');
    }
    const basic = await post({ token_endpoint_auth_method: 'client_secret_basic' });
    assert.equal(basic.status, 201, await basic.clone().text());
    assert.ok((await basic.json()).client_secret);
    const ip = freshIp();
    for (let i = 0; i < 20; i++) assert.equal((await post({ token_endpoint_auth_method: 'none' }, ip)).status, 201, `registration ${i + 1}`);
    const over = await post({ token_endpoint_auth_method: 'none' }, ip);
    assert.equal(over.status, 429);
    assert.equal((await over.json()).error, 'too_many_requests');
    // A client that never completed a grant goes after a week; one that did stays.
    const email = 'oauth-sweep@example.test';
    h.canRegister.state.allowed.add(email);
    const idle = await register(h);
    const used = await consentFlow(h, { email, scope: 'runbooks:read' });
    const weekAgo = Date.now() - 8 * 24 * 3600 * 1000;
    await h.backdateClient(idle.client_id, weekAgo);
    await h.backdateClient(used.client.client_id, weekAgo);
    await register(h);
    assert.equal(await h.clientExists(idle.client_id), false, 'the idle client is gone');
    assert.equal(await h.clientExists(used.client.client_id), true, 'a client with a grant stays');
  });

  const internal = async (route, body) => {
    const r = await req(h, `/internal/oauth/${route}`, { method: 'POST', headers: { 'content-type': 'application/json', 'x-runbook-internal-secret': INTERNAL_SECRET }, body: JSON.stringify(body) });
    return { status: r.status, body: await r.json() };
  };

  test('OAuth: a person lists and revokes a connected app; its tokens end at once (re-check 1)', async () => {
    const email = 'oauth-apps@example.test';
    h.canRegister.state.allowed.add(email);
    const flow = await consentFlow(h, { email, scope: 'runbooks:read offline_access' });
    const tokens = await (await exchange(h, flow)).json();
    const who = (await introspect(h, tokens.access_token)).body.auth_user_id;
    const listed = await internal('grants', { auth_user_ids: [who] });
    assert.equal(listed.status, 200);
    const grant = listed.body.grants.find((g) => g.client_id === flow.client.client_id);
    assert.ok(grant, JSON.stringify(listed.body));
    assert.equal(grant.client_name, 'Test MCP client');
    assert.deepEqual(grant.redirect_hosts, ['127.0.0.1:53682 (an app on this computer)']);
    assert.deepEqual([...grant.scopes].sort(), ['offline_access', 'runbooks:read']);
    assert.equal(typeof grant.created_at, 'number');
    assert.equal(typeof grant.last_used_at, 'number');
    assert.equal(grant.expires_at - (grant.created_at > 0 ? grant.expires_at - 90 * 24 * 3600 * 1000 : 0), 90 * 24 * 3600 * 1000);
    assert.deepEqual((await internal('grants', { auth_user_ids: ['someone-else'] })).body.grants, []);
    const revoked = await internal('grants/revoke', { auth_user_ids: [who], client_id: flow.client.client_id });
    assert.deepEqual(revoked.body, { revoked: true });
    assert.equal((await introspect(h, tokens.access_token)).active, false, 'the access token ends');
    const refresh = await tokenRequest(h, { grant_type: 'refresh_token', client_id: flow.client.client_id, refresh_token: tokens.refresh_token });
    assert.equal(refresh.status, 400, 'the refresh token ends');
    assert.equal((await refresh.json()).error, 'invalid_grant');
    assert.equal((await internal('grants', { auth_user_ids: [who] })).body.grants.some((g) => g.client_id === flow.client.client_id), false);
    assert.deepEqual((await internal('grants/revoke', { auth_user_ids: [who], client_id: flow.client.client_id })).body, { revoked: false });
    // Only the person's own grant: another user's ID revokes nothing.
    const other = await consentFlow(h, { email, scope: 'runbooks:read', cookie: flow.cookie });
    const otherTokens = await (await exchange(h, other)).json();
    assert.deepEqual((await internal('grants/revoke', { auth_user_ids: ['someone-else'], client_id: other.client.client_id })).body, { revoked: false });
    assert.equal((await introspect(h, otherTokens.access_token)).active, true);
    assert.equal((await internal('grants', { auth_user_ids: [who] })).status, 200);
    const noSecret = await req(h, '/internal/oauth/grants', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' });
    assert.equal(noSecret.status, 401);
    // Re-check 3: more identities than the cap are refused, never cut short.
    const many = [who, ...Array.from({ length: 64 }, (_, i) => `extra-${i}`)];
    assert.notEqual((await internal('grants/revoke', { auth_user_ids: many, client_id: other.client.client_id })).status, 200);
    assert.notEqual((await internal('grants', { auth_user_ids: many })).status, 200);
    assert.equal((await introspect(h, otherTokens.access_token)).active, true);
  });

  test('OAuth: a grant ends 90 days after the last consent (re-check 1)', async () => {
    const email = 'oauth-old@example.test';
    h.canRegister.state.allowed.add(email);
    const flow = await consentFlow(h, { email, scope: 'runbooks:read offline_access' });
    const tokens = await (await exchange(h, flow)).json();
    await h.backdateConsent(flow.client.client_id, Date.now() - 89 * 24 * 3600 * 1000);
    assert.equal((await introspect(h, tokens.access_token)).active, true, 'day 89');
    await h.backdateConsent(flow.client.client_id, Date.now() - 91 * 24 * 3600 * 1000);
    assert.equal((await introspect(h, tokens.access_token)).active, false, 'day 91');
    const refresh = await tokenRequest(h, { grant_type: 'refresh_token', client_id: flow.client.client_id, refresh_token: tokens.refresh_token });
    assert.equal(refresh.status, 400);
    assert.equal((await refresh.json()).error, 'invalid_grant');
  });

  test('OAuth: a token request that repeats grant_type or refresh_token is refused (re-check 3)', async () => {
    const email = 'oauth-dup@example.test';
    h.canRegister.state.allowed.add(email);
    const flow = await consentFlow(h, { email, scope: 'runbooks:read offline_access' });
    const tokens = await (await exchange(h, flow)).json();
    await h.backdateConsent(flow.client.client_id, Date.now() - 91 * 24 * 3600 * 1000);
    const form = new URLSearchParams([
      ['grant_type', 'authorization_code'],
      ['grant_type', 'refresh_token'],
      ['client_id', flow.client.client_id],
      ['refresh_token', tokens.refresh_token],
    ]);
    const smuggled = await req(h, '/api/auth/oauth2/token', {
      method: 'POST',
      headers: { 'content-type': 'application/x-www-form-urlencoded', [h.ipHeader]: freshIp() },
      body: form.toString(),
    });
    assert.equal(smuggled.status, 400);
    assert.equal((await smuggled.json()).error, 'invalid_request');
    const twice = await req(h, '/api/auth/oauth2/token', {
      method: 'POST',
      headers: { 'content-type': 'application/x-www-form-urlencoded', [h.ipHeader]: freshIp() },
      body: new URLSearchParams([
        ['grant_type', 'refresh_token'],
        ['client_id', flow.client.client_id],
        ['refresh_token', 'not-a-token'],
        ['refresh_token', tokens.refresh_token],
      ]).toString(),
    });
    assert.equal(twice.status, 400);
    assert.equal((await twice.json()).error, 'invalid_request');
  });

  test('OAuth: a backend failure is 503, not inactive; a deleted client or user ends the token (re-check 2)', async () => {
    const email = 'oauth-broken@example.test';
    h.canRegister.state.allowed.add(email);
    const flow = await consentFlow(h, { email, scope: 'runbooks:read' });
    const tokens = await (await exchange(h, flow)).json();
    await h.breakTokens();
    try {
      const r = await introspect(h, tokens.access_token);
      assert.equal(r.status, 503, JSON.stringify(r.body));
      assert.equal(r.body.code, 'auth.unavailable');
    } finally {
      await h.fixTokens();
    }
    assert.equal((await introspect(h, tokens.access_token)).active, true);
    await h.deleteClient(flow.client.client_id);
    assert.equal((await introspect(h, tokens.access_token)).active, false, 'deleted client');
    const second = 'oauth-gone@example.test';
    h.canRegister.state.allowed.add(second);
    const flow2 = await consentFlow(h, { email: second, scope: 'runbooks:read' });
    const tokens2 = await (await exchange(h, flow2)).json();
    await h.deleteUser(second);
    assert.equal((await introspect(h, tokens2.access_token)).active, false, 'deleted user');
  });

  test('OAuth: prompt=login does not loop back to sign-in (re-check 6)', async () => {
    const email = 'oauth-login@example.test';
    h.canRegister.state.allowed.add(email);
    const client = await register(h);
    const start = await req(h, authorizeUrl(client, { challenge: pkce().challenge, scope: 'runbooks:read', extra: { prompt: 'login' } }));
    assert.equal(new URL(start.headers.get('location'), ORIGIN).pathname, '/api/auth/oauth/sign-in');
    const signedIn = await signInPage(h, start.headers.get('location'), email);
    assert.ok(!/[?&]prompt=[^&]*login/.test(signedIn.next), signedIn.next);
    const r = await req(h, signedIn.next, { headers: { cookie: signedIn.cookie } });
    assert.equal(new URL(r.headers.get('location'), ORIGIN).pathname, '/api/auth/oauth/consent');
  });

  test('OAuth: a registration without a client address is refused; blank names show as unnamed (re-check 7, 8)', async () => {
    // Natively the host always sets x-runbook-client-ip; Cloudflare (and
    // Miniflare) always set cf-connecting-ip, so only the server can be asked.
    if (h.ipHeader === 'x-runbook-client-ip') {
      const r = await h.request('/api/auth/oauth2/register', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ client_name: 'x', redirect_uris: [REDIRECT], token_endpoint_auth_method: 'none' }),
      });
      assert.equal(r.status, 400);
      assert.equal((await r.json()).error, 'invalid_request');
    }
    const email = 'oauth-blank@example.test';
    h.canRegister.state.allowed.add(email);
    const blank = await consentFlow(h, { email, scope: 'runbooks:read', client: await register(h, { client_name: 'ㅤ⠀ᅟᅠ​' }) });
    assert.match(blank.html, /<h1>Allow Unnamed app to use The Runbook\?<\/h1>/);
    const zalgo = await consentFlow(h, { email, scope: 'runbooks:read', cookie: blank.cookie, client: await register(h, { client_name: `Z${'̶'.repeat(40)}app Á` }) });
    assert.match(zalgo.html, /<h1>Allow Zapp Á to use The Runbook\?<\/h1>/);
  });

  test('OAuth: no request left the machine', () => {
    assert.deepEqual(h.outbound.unexpected, []);
  });
}
