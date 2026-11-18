// MCP2: OAuth 2.1 for remote MCP clients on the Cloudflare edition, end to
// end: the api Worker serves the protected resource metadata and `/mcp`, the
// auth Worker (Better Auth's OAuth provider on the same D1) is the
// authorization server behind the AUTH binding, and an access token is
// introspected by the auth Worker's internal route (cached 30 s) and linked
// to its directory user (D45). LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { startSignInStack } from './harness.mjs';

const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');
const OWNER = 'owner@example.test';
const REDIRECT = 'http://127.0.0.1:53682/callback';
const SOURCE = 'apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: oauth-check, title: OAuth check}\nspec:\n  steps:\n    - id: check\n      title: Check\n      actor: agent\n      instructions: Check.\n';
let stack;
let pat;
before(async () => {
  stack = await startSignInStack({ vars: { BOOTSTRAP_SECRET } });
  const boot = await stack.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops', owner_email: OWNER }),
  });
  assert.equal(boot.status, 201, await boot.clone().text());
  pat = (await boot.json()).personal_token;
});
after(async () => {
  await stack?.dispose();
});

const form = (path, fields, headers = {}) =>
  stack.fetch(path, { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded', ...headers }, body: new URLSearchParams(fields).toString() });
const rel = (location) => {
  const url = new URL(location, stack.origin);
  return `${url.pathname}${url.search}`;
};
const INIT = { jsonrpc: '2.0', id: 0, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 't', version: '1' } } };
const tool = (name, args) => ({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args } });
const mcp = (bearer, message, extra = {}) =>
  stack.fetch('/mcp', {
    method: 'POST',
    headers: { 'content-type': 'application/json', accept: 'application/json', ...(bearer ? { authorization: `Bearer ${bearer}` } : {}), ...extra },
    body: JSON.stringify(message),
  });
async function result(response) {
  const text = await response.text();
  assert.equal(response.status, 200, text);
  return JSON.parse(text).result;
}

/** Register, authorize with PKCE, sign in on the page, consent ticking `tick`, exchange. */
async function connect(scope, tick, existingCookie = null) {
  const reg = await stack.fetch('/api/auth/oauth2/register', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ client_name: 'Workers test client', redirect_uris: [REDIRECT], token_endpoint_auth_method: 'none' }),
  });
  assert.equal(reg.status, 201, await reg.clone().text());
  const client = await reg.json();
  const verifier = randomBytes(32).toString('base64url');
  const challenge = createHash('sha256').update(verifier).digest('base64url');
  const params = new URLSearchParams({ response_type: 'code', client_id: client.client_id, redirect_uri: REDIRECT, state: 's1', code_challenge: challenge, code_challenge_method: 'S256', scope, resource: `${stack.origin}/mcp` });
  const origin = { origin: stack.origin };
  let cookie = existingCookie;
  let consentPage;
  if (cookie) {
    // Already signed in in this browser: straight to the consent page.
    const start = await stack.fetch(`/api/auth/oauth2/authorize?${params}`, { headers: { cookie } });
    assert.equal(start.status, 302, await start.clone().text());
    consentPage = start.headers.get('location');
  } else {
    const start = await stack.fetch(`/api/auth/oauth2/authorize?${params}`);
    assert.equal(start.status, 302, await start.clone().text());
    const signInPage = start.headers.get('location');
    assert.equal(new URL(signInPage).pathname, '/api/auth/oauth/sign-in');
    const query = new URL(signInPage).search.slice(1);
    const sent = await form('/api/auth/oauth/sign-in', { oauth_query: query, email: OWNER }, origin);
    assert.equal(sent.status, 200, await sent.clone().text());
    const code = (await stack.mail()).filter((m) => m.to === OWNER).at(-1).code;
    const signed = await form('/api/auth/oauth/sign-in', { oauth_query: query, email: OWNER, otp: code }, origin);
    assert.equal(signed.status, 303, await signed.clone().text());
    cookie = signed.headers.getSetCookie().map((c) => c.split(';')[0]).join('; ');
    const resumed = await stack.fetch(signed.headers.get('location'), { headers: { cookie } });
    assert.equal(resumed.status, 302, await resumed.clone().text());
    consentPage = resumed.headers.get('location');
  }
  assert.equal(new URL(consentPage).pathname, '/api/auth/oauth/consent');
  const page = await stack.fetch(rel(consentPage), { headers: { cookie } });
  assert.equal(page.status, 200);
  const html = await page.text();
  const fields = new URLSearchParams({ oauth_query: new URL(consentPage).search.slice(1), decision: 'allow' });
  for (const s of tick) fields.append('scope', s);
  const decided = await stack.fetch('/api/auth/oauth/consent', { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded', cookie, ...origin }, body: fields.toString() });
  assert.equal(decided.status, 303, await decided.clone().text());
  const back = new URL(decided.headers.get('location'));
  assert.equal(`${back.origin}${back.pathname}`, REDIRECT);
  const token = await form('/api/auth/oauth2/token', { grant_type: 'authorization_code', client_id: client.client_id, code: back.searchParams.get('code'), code_verifier: verifier, redirect_uri: REDIRECT, resource: `${stack.origin}/mcp` });
  assert.equal(token.status, 200, await token.clone().text());
  return { client, html, cookie, ...(await token.json()) };
}

const METADATA = () => `${stack.origin}/.well-known/oauth-protected-resource/mcp`;

test('discovery: /mcp points at the protected resource metadata, which points at the auth Worker', async () => {
  const r = await mcp(null, INIT);
  assert.equal(r.status, 401);
  assert.equal(r.headers.get('www-authenticate'), `Bearer resource_metadata="${METADATA()}"`);
  for (const path of ['/.well-known/oauth-protected-resource', '/.well-known/oauth-protected-resource/mcp']) {
    const prm = await stack.fetch(path);
    assert.equal(prm.status, 200, path);
    const doc = await prm.json();
    assert.equal(doc.resource, `${stack.origin}/mcp`);
    assert.deepEqual(doc.authorization_servers, [`${stack.origin}/api/auth`]);
  }
  for (const path of ['/.well-known/oauth-authorization-server', '/.well-known/oauth-authorization-server/api/auth']) {
    const asm = await stack.fetch(path);
    assert.equal(asm.status, 200, await asm.clone().text());
    const doc = await asm.json();
    assert.equal(doc.issuer, `${stack.origin}/api/auth`);
    assert.equal(doc.token_endpoint, `${stack.origin}/api/auth/oauth2/token`);
    assert.deepEqual(doc.code_challenge_methods_supported, ['S256']);
  }
});

let owner;
test('the full flow: register, PKCE, sign-in, consent, token, then /mcp tools with the token', async () => {
  owner = await connect('runbooks:read runbooks:write runbooks:publish executions:read offline_access', ['runbooks:read', 'runbooks:write', 'executions:read', 'offline_access']);
  assert.match(owner.html, /value="runbooks:publish">/, 'publish is shown unticked');
  assert.match(owner.access_token, /^trb_oat_/);
  assert.ok(!owner.scope.includes('runbooks:publish'), owner.scope);
  assert.equal((await mcp(owner.access_token, INIT)).status, 200);
  const listed = await result(await mcp(owner.access_token, tool('list_runbooks', { project: 'ops' })));
  assert.notEqual(listed.isError, true, JSON.stringify(listed));
  const saved = await result(await mcp(owner.access_token, tool('save_runbook_draft', { source: SOURCE, expected_revision: 0, project: 'ops' })));
  assert.notEqual(saved.isError, true, JSON.stringify(saved));
  // runbooks:publish was not ticked: the owner's role allows it, the grant does not.
  const published = await result(await mcp(owner.access_token, tool('publish_runbook', { runbook: 'oauth-check', project: 'ops', revision: 1 })));
  assert.ok(JSON.stringify(published).includes('ask_human_to_publish'), JSON.stringify(published));
  // The token is bound to /mcp: the REST API refuses it.
  const rest = await stack.fetch('/v1/orgs', { headers: { authorization: `Bearer ${owner.access_token}` } });
  assert.equal(rest.status, 401);
});

test('refresh works; a revoked, an expired or an unknown token is refused with invalid_token', async () => {
  const refreshed = await form('/api/auth/oauth2/token', { grant_type: 'refresh_token', client_id: owner.client.client_id, refresh_token: owner.refresh_token });
  assert.equal(refreshed.status, 200, await refreshed.clone().text());
  const next = await refreshed.json();
  assert.equal((await mcp(next.access_token, INIT)).status, 200);
  // Revoked before its first use (nothing cached): refused.
  const again = await form('/api/auth/oauth2/token', { grant_type: 'refresh_token', client_id: owner.client.client_id, refresh_token: next.refresh_token });
  const third = await again.json();
  const revoked = await form('/api/auth/oauth2/revoke', { token: third.access_token, token_type_hint: 'access_token', client_id: owner.client.client_id });
  assert.equal(revoked.status, 200, await revoked.clone().text());
  const r = await mcp(third.access_token, INIT);
  assert.equal(r.status, 401);
  assert.equal(r.headers.get('www-authenticate'), `Bearer resource_metadata="${METADATA()}", error="invalid_token"`);
  // Expired before its first use.
  const fourth = await (await form('/api/auth/oauth2/token', { grant_type: 'refresh_token', client_id: owner.client.client_id, refresh_token: third.refresh_token })).json();
  await stack.db.prepare('UPDATE auth_oauth_access_token SET expires_at = ?').bind(Date.now() - 1000).run();
  assert.equal((await mcp(fourth.access_token, INIT)).status, 401);
  assert.equal((await mcp('trb_oat_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', INIT)).status, 401);
});

test('a cookie alone never authenticates /mcp, and a PAT still does', async () => {
  const r = await mcp(null, INIT, { cookie: owner.cookie });
  assert.equal(r.status, 401);
  assert.equal(r.headers.get('www-authenticate'), `Bearer resource_metadata="${METADATA()}"`);
  assert.equal((await mcp(pat, INIT)).status, 200);
});

test('ticking runbooks:publish grants it', async () => {
  const ticked = await connect('runbooks:read runbooks:publish', ['runbooks:read', 'runbooks:publish'], owner.cookie);
  assert.ok(ticked.scope.split(' ').includes('runbooks:publish'), ticked.scope);
  const published = await result(await mcp(ticked.access_token, tool('publish_runbook', { runbook: 'oauth-check', project: 'ops', revision: 1 })));
  assert.notEqual(published.isError, true, JSON.stringify(published));
});

test('connected apps: the person lists a grant and disconnects it; its tokens stop (re-check 1)', async () => {
  const app = await connect('runbooks:read offline_access', ['runbooks:read', 'offline_access'], owner.cookie);
  const listed = await stack.fetch('/v1/me/apps', { headers: { cookie: app.cookie } });
  assert.equal(listed.status, 200, await listed.clone().text());
  const item = (await listed.json()).items.find((a) => a.client_id === app.client.client_id);
  assert.ok(item);
  assert.equal(item.client_name, 'Workers test client');
  assert.deepEqual(item.redirect_hosts, ['127.0.0.1:53682 (an app on this computer)']);
  assert.ok(item.expires_at_ms > item.created_at_ms);
  // The PAT person (the owner, tokens:manage) sees the same grants.
  const viaPat = await stack.fetch('/v1/me/apps', { headers: { authorization: `Bearer ${pat}` } });
  assert.equal(viaPat.status, 200);
  assert.ok((await viaPat.json()).items.some((a) => a.client_id === app.client.client_id));
  // An OAuth token never manages connected apps (it is no REST bearer).
  assert.equal((await stack.fetch('/v1/me/apps', { headers: { authorization: `Bearer ${app.access_token}` } })).status, 401);
  const gone = await stack.fetch(`/v1/me/apps/${app.client.client_id}`, {
    method: 'DELETE',
    headers: { cookie: app.cookie, origin: stack.origin, 'x-runbook-csrf': '1' },
  });
  assert.equal(gone.status, 204, await gone.clone().text());
  // This isolate's cache was cleared; the access and refresh tokens are revoked.
  assert.equal((await mcp(app.access_token, INIT)).status, 401);
  const refreshed = await form('/api/auth/oauth2/token', { grant_type: 'refresh_token', client_id: app.client.client_id, refresh_token: app.refresh_token });
  assert.equal(refreshed.status, 400);
  assert.equal((await stack.fetch(`/v1/me/apps/${app.client.client_id}`, { method: 'DELETE', headers: { cookie: app.cookie, origin: stack.origin, 'x-runbook-csrf': '1' } })).status, 404);
});

/** How the auth Worker stores an access token: base64url(SHA-256) without the prefix. */
const tokenHash = (token) => createHash('sha256').update(token.slice('trb_oat_'.length)).digest('base64url');

test('against the real auth Worker: another resource is refused, a dead token is remembered briefly, sign-out keeps the grant (review 4, 6, 7)', async () => {
  const fresh = await connect('runbooks:read offline_access', ['runbooks:read', 'offline_access'], owner.cookie);
  const refresh = async (token) => {
    const r = await form('/api/auth/oauth2/token', { grant_type: 'refresh_token', client_id: fresh.client.client_id, refresh_token: token });
    assert.equal(r.status, 200, await r.clone().text());
    return r.json();
  };
  // Audience: the token row bound to another resource is refused, because
  // the api Worker names its own /mcp and the auth Worker checks it.
  await stack.db
    .prepare('UPDATE auth_oauth_access_token SET resources = replace(resources, ?, ?) WHERE token = ?')
    .bind(`${stack.origin}/mcp`, 'https://elsewhere.example/mcp', tokenHash(fresh.access_token))
    .run();
  assert.equal((await mcp(fresh.access_token, INIT)).status, 401);
  // Negative cache: "not active" is remembered for a few seconds.
  const next = await refresh(fresh.refresh_token);
  const dead = `trb_oat_${'Q'.repeat(32)}`;
  assert.equal((await mcp(dead, INIT)).status, 401);
  await stack.db.prepare('UPDATE auth_oauth_access_token SET token = ? WHERE token = ?').bind(tokenHash(dead), tokenHash(next.access_token)).run();
  assert.equal((await mcp(dead, INIT)).status, 401, 'still remembered as inactive');
  await new Promise((r) => setTimeout(r, 5_500));
  assert.equal((await mcp(dead, INIT)).status, 200, 'asked again after the window');
  // Signing out of the web app keeps the grant: refresh, and its token works.
  const out = await stack.fetch('/api/auth/sign-out', { method: 'POST', headers: { origin: stack.origin, cookie: fresh.cookie, 'content-type': 'application/json' }, body: '{}' });
  assert.equal(out.status, 200, await out.clone().text());
  const after = await refresh(next.refresh_token);
  assert.equal((await mcp(after.access_token, INIT)).status, 200);
  // An OAuth token is no bearer on the REST API: no device approval, no
  // execution credentials.
  for (const path of ['/v1/device/approve', '/v1/executions/exe_00000000000000000000000001/tokens']) {
    const r = await stack.fetch(path, {
      method: 'POST',
      headers: { authorization: `Bearer ${after.access_token}`, 'content-type': 'application/json', 'idempotency-key': 'oauth-rest' },
      body: JSON.stringify({ user_code: 'ABCD-EFGH', approve: true }),
    });
    assert.ok([401, 403].includes(r.status), `${path}: ${r.status}`);
  }
});
