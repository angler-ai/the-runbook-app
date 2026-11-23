import { readdirSync, readFileSync } from 'node:fs';
import { dirname as contractDir, join } from 'node:path';
import { fileURLToPath as contractPath } from 'node:url';
const CONTRACT = join(contractDir(contractPath(import.meta.url)), '..', '..', '..', 'api', 'internal');
// The AUTH1 fail-first behaviours, run once on Miniflare/D1 (tests/d1.test.mjs)
// and once on Node/Postgres (tests/pg.test.mjs). A harness `h` provides:
//   request(path, init)        -> Response from the service (redirects not followed)
//   ipHeader                   'cf-connecting-ip' | 'x-runbook-client-ip'
//   mail()                     -> [{to, code}] delivered so far
//   count(table, email)        -> rows in user/session/verification for an email
//   setSessionExpiry(id, ms)   -> rewrites a session's expiry
//   resetLimits()              -> clears the send-code limit table
//   verificationValue(email)   -> the stored sign-in code record, or null
//   canRegister, google, outbound   the stubs from ./stubs.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import { INTERNAL_SECRET, ORIGIN } from './stubs.mjs';

let ipCounter = 0;
const freshIp = () => `198.51.100.${++ipCounter}`;
const SESSION_COOKIE = '__Host-runbook.session_token';
/** The name before gate fix 7, accepted for one release. */
const LEGACY_COOKIE = '__Secure-runbook.session_token';

export function post(h, path, body, { ip = freshIp(), origin = ORIGIN, cookie, headers = {} } = {}) {
  return h.request(`/api/auth${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...headers, ...(origin ? { origin } : {}), [h.ipHeader]: ip, ...(cookie ? { cookie } : {}) },
    body: JSON.stringify(body),
  });
}

/** `name=value` pairs from Set-Cookie headers, for a Cookie header. */
export const cookieHeader = (response) => response.headers.getSetCookie().map((c) => c.split(';')[0]).join('; ');
const sessionSetCookie = (response) => response.headers.getSetCookie().find((c) => c.startsWith(`${SESSION_COOKIE}=`));

const latestCode = (h, email) => h.mail().filter((m) => m.to === email).at(-1)?.code;
const wrong = (code) => String((Number(code) + 1) % 1_000_000).padStart(6, '0');

async function sendCode(h, email, ip = freshIp(), headers = {}) {
  const response = await post(h, '/email-otp/send-verification-otp', { email, type: 'sign-in' }, { ip, headers });
  return response;
}

/** Signs in by email code and returns {response, cookie}. */
export async function signInByEmail(h, email) {
  const ip = freshIp();
  const sent = await sendCode(h, email, ip);
  assert.equal(sent.status, 200, `send-code for ${email}: ${await sent.clone().text()}`);
  const code = latestCode(h, email);
  assert.match(code, /^\d{6}$/);
  const response = await post(h, '/sign-in/email-otp', { email, otp: code }, { ip });
  return { response, cookie: cookieHeader(response), code, ip };
}

const internalSession = (h, cookie, secret = INTERNAL_SECRET) =>
  h.request('/internal/session', { method: 'POST', headers: { ...(secret ? { 'x-runbook-internal-secret': secret } : {}), ...(cookie ? { cookie } : {}) } });

/** Starts Google sign-in and completes the callback with `profile`. */
async function googleSignIn(h, profile) {
  const ip = freshIp();
  const start = await post(h, '/sign-in/social', { provider: 'google', callbackURL: '/runs' }, { ip });
  assert.equal(start.status, 200, await start.clone().text());
  const { url } = await start.json();
  const authorize = new URL(url);
  assert.equal(authorize.origin, 'https://accounts.google.com');
  assert.equal(authorize.searchParams.get('client_id'), 'test-client.apps.example.test');
  h.google.state.next = { ...profile, nonce: authorize.searchParams.get('nonce') ?? undefined };
  const callback = `/api/auth/callback/google?code=test-code&state=${encodeURIComponent(authorize.searchParams.get('state'))}`;
  return h.request(callback, { headers: { cookie: cookieHeader(start), [h.ipHeader]: ip } });
}

export function behaviourSuite(h) {
  test('password routes return 404', async () => {
    const before = await h.count('user', 'pat@example.test');
    for (const path of ['/sign-up/email', '/sign-in/email', '/request-password-reset', '/forget-password', '/reset-password', '/change-password', '/set-password', '/email-otp/reset-password', '/email-otp/request-password-reset', '/forget-password/email-otp', '/update-user', '/change-email', '/delete-user', '/verify-email', '/email-otp/check-verification-otp', '/email-otp/verify-email']) {
      const r = await post(h, path, { email: 'pat@example.test', password: 'long-test-password-123', newPassword: 'long-test-password-456', name: 'Pat', token: 'x', otp: '000000' });
      assert.equal(r.status, 404, `${path} -> ${r.status} ${await r.text()}`);
    }
    assert.equal((await h.request('/api/auth/reset-password/some-token')).status, 404);
    assert.equal(await h.count('user', 'pat@example.test'), before);
  });

  test('the 6th wrong code invalidates the code', async () => {
    const email = 'six@example.test';
    h.canRegister.state.allowed.add(email);
    const ip = freshIp();
    assert.equal((await sendCode(h, email, ip)).status, 200);
    const code = latestCode(h, email);
    const stored = await h.verificationValue(email);
    assert.ok(stored && !stored.includes(code), 'the code is stored hashed');
    for (let i = 1; i <= 5; i++) {
      const r = await post(h, '/sign-in/email-otp', { email, otp: wrong(code) }, { ip });
      assert.equal(r.status, 400, `wrong attempt ${i}`);
      assert.equal((await r.json()).code, 'INVALID_OTP');
    }
    const sixth = await post(h, '/sign-in/email-otp', { email, otp: wrong(code) }, { ip });
    assert.equal(sixth.status, 403, 'the 6th wrong code is refused as too many attempts');
    assert.equal((await sixth.json()).code, 'TOO_MANY_ATTEMPTS');
    const right = await post(h, '/sign-in/email-otp', { email, otp: code }, { ip });
    assert.notEqual(right.status, 200, 'the right code no longer works');
    assert.equal(sessionSetCookie(right), undefined);
    assert.equal(await h.count('user', email), 0);
    assert.equal(await h.count('verification', email), 0, 'the code is gone');
  });

  test('five wrong codes leave the right code unusable', async () => {
    const email = 'five@example.test';
    h.canRegister.state.allowed.add(email);
    const ip = freshIp();
    assert.equal((await sendCode(h, email, ip)).status, 200);
    const code = latestCode(h, email);
    for (let i = 0; i < 5; i++) assert.equal((await post(h, '/sign-in/email-otp', { email, otp: wrong(code) }, { ip })).status, 400);
    const right = await post(h, '/sign-in/email-otp', { email, otp: code }, { ip });
    assert.equal(right.status, 403);
    assert.equal(sessionSetCookie(right), undefined);
  });

  test('a used code is refused', async () => {
    const email = 'once@example.test';
    h.canRegister.state.allowed.add(email);
    const first = await signInByEmail(h, email);
    assert.equal(first.response.status, 200, await first.response.clone().text());
    assert.ok(sessionSetCookie(first.response));
    const again = await post(h, '/sign-in/email-otp', { email, otp: first.code }, { ip: first.ip });
    assert.equal(again.status, 400);
    assert.equal((await again.json()).code, 'INVALID_OTP');
    assert.equal(sessionSetCookie(again), undefined);
    assert.equal(await h.count('session', email), 1);
  });

  test('the send-code response is identical for known and unknown emails', async () => {
    const known = 'known@example.test';
    const unknown = 'nobody@example.test';
    h.canRegister.state.allowed.add(known);
    assert.equal((await signInByEmail(h, known)).response.status, 200);
    assert.equal(await h.count('user', known), 1);
    assert.equal(await h.count('user', unknown), 0);
    await h.resetLimits();
    const shape = async (r) => ({ status: r.status, body: await r.text(), headers: [...r.headers.keys()].filter((k) => k !== 'date').sort().join(',') });
    const [a, b] = [await shape(await sendCode(h, known)), await shape(await sendCode(h, unknown))];
    assert.deepEqual(a, b);
    assert.equal(a.status, 200);
    assert.ok(latestCode(h, unknown), 'an unknown address gets a code too; refusal waits for can-register');
    // A second request inside the 60 s gap is limited the same way for both.
    const [c, d] = [await shape(await sendCode(h, known)), await shape(await sendCode(h, unknown))];
    assert.deepEqual(c, d);
    assert.equal(c.status, 429);
    assert.equal(h.canRegister.state.calls.filter((call) => call.email === unknown).length, 0, 'sending a code never asks can-register');
  });

  test('a disallowed domain is refused', async () => {
    const email = 'eve@blocked.example.test';
    h.canRegister.state.allowed.add(email);
    const sent = await sendCode(h, email);
    assert.equal(sent.status, 403);
    assert.equal((await sent.json()).code, 'auth.domain_not_allowed');
    assert.equal(h.mail().filter((m) => m.to === email).length, 0, 'no code is sent');
    const signIn = await post(h, '/sign-in/email-otp', { email, otp: '123456' });
    assert.equal(signIn.status, 403);
    const viaGoogle = await googleSignIn(h, { sub: 'google-eve', email, email_verified: true });
    assert.equal(viaGoogle.status, 302);
    assert.equal(sessionSetCookie(viaGoogle), undefined);
    assert.equal(await h.count('user', email), 0);
  });

  test('Google with email_verified=false is refused', async () => {
    const email = 'gina@example.test';
    h.canRegister.state.allowed.add(email);
    const calls = h.google.state.calls.length;
    const refused = await googleSignIn(h, { sub: 'google-gina', email, email_verified: false });
    assert.equal(refused.status, 302);
    const location = new URL(refused.headers.get('location'));
    assert.equal(`${location.origin}${location.pathname}`, `${ORIGIN}/sign-in`);
    assert.ok(location.searchParams.get('error'));
    assert.equal(sessionSetCookie(refused), undefined);
    assert.equal(await h.count('user', email), 0);
    assert.deepEqual(h.google.state.calls.slice(calls), ['token', 'jwks'], 'the code was exchanged and the ID token checked against the JWKS');
    // Control: the same account with a verified email signs in.
    const accepted = await googleSignIn(h, { sub: 'google-gina', email, email_verified: true });
    assert.equal(accepted.status, 302, await accepted.clone().text());
    assert.equal(accepted.headers.get('location'), '/runs');
    assert.ok(sessionSetCookie(accepted));
    const session = await internalSession(h, cookieHeader(accepted));
    assert.equal(session.status, 200);
    // Google's name (and photo, when the token has one) ride along for the account.
    assert.deepEqual({ ...(await session.json()), auth_user_id: undefined, session_id: undefined, expires_at: undefined }, { auth_user_id: undefined, email, email_verified: true, mfa_verified_at: null, mfa_method: null, mfa_enrolled: false, name: 'Test Person', session_id: undefined, expires_at: undefined });
  });

  test('a Google ID token not signed by Google is refused', async () => {
    const email = 'mallory@example.test';
    h.canRegister.state.allowed.add(email);
    const r = await googleSignIn(h, { sub: 'google-mallory', email, email_verified: true, forge: true });
    assert.equal(r.status, 302);
    assert.equal(sessionSetCookie(r), undefined);
    assert.equal(await h.count('user', email), 0);
  });

  test('email and Google resolve to the same account', async () => {
    const email = 'both@example.test';
    h.canRegister.state.allowed.add(email);
    const byEmail = await signInByEmail(h, email);
    assert.equal(byEmail.response.status, 200);
    const byGoogle = await googleSignIn(h, { sub: 'google-both', email, email_verified: true });
    assert.equal(byGoogle.status, 302);
    const a = await (await internalSession(h, byEmail.cookie)).json();
    const b = await (await internalSession(h, cookieHeader(byGoogle))).json();
    assert.equal(a.auth_user_id, b.auth_user_id);
    assert.equal(await h.count('user', email), 1);
  });

  test('the session cookie has the right flags and renews', async () => {
    const email = 'cookie@example.test';
    h.canRegister.state.allowed.add(email);
    const { response, cookie } = await signInByEmail(h, email);
    assert.equal(response.status, 200);
    const set = sessionSetCookie(response);
    assert.ok(set, `expected ${SESSION_COOKIE} in ${response.headers.getSetCookie().join(' | ')}`);
    const attrs = set.split(';').slice(1).map((a) => a.trim().toLowerCase());
    for (const flag of ['httponly', 'secure', 'samesite=lax', 'path=/', 'max-age=2592000']) assert.ok(attrs.includes(flag), `${flag} in ${set}`);
    assert.ok(!attrs.some((a) => a.startsWith('domain=')), 'host-only cookie');
    // Sliding renewal: a session older than a day is pushed back to 30 days.
    const resolved = await (await internalSession(h, cookie)).json();
    await h.setSessionExpiry(resolved.session_id, Date.now() + 2 * 24 * 3600 * 1000);
    const renewed = await h.request('/api/auth/get-session', { headers: { cookie, [h.ipHeader]: freshIp() } });
    assert.equal(renewed.status, 200);
    assert.ok(sessionSetCookie(renewed), 'the cookie is re-issued');
    const after = await (await internalSession(h, cookie)).json();
    assert.ok(after.expires_at > Date.now() + 29 * 24 * 3600 * 1000, 'expiry moved back to 30 days');
  });

  test('a session cookie under the pre-__Host- name still works for one release', async () => {
    // Gate fix 7: https cookies moved from __Secure- to __Host-; a browser that
    // signed in before still holds the old name.
    const email = 'legacy@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const value = cookie.split('; ').find((c) => c.startsWith(`${SESSION_COOKIE}=`)).slice(SESSION_COOKIE.length + 1);
    const legacy = `${LEGACY_COOKIE}=${value}`;
    const resolved = await internalSession(h, legacy);
    assert.equal(resolved.status, 200, await resolved.clone().text());
    assert.equal((await resolved.json()).email, email);
    const session = await h.request('/api/auth/get-session', { headers: { cookie: legacy, [h.ipHeader]: freshIp() } });
    assert.equal(session.status, 200);
    assert.equal((await session.json())?.user?.email, email);
    // Signing out with the old name ends the session and expires both names.
    const out = await h.request('/api/auth/sign-out', { method: 'POST', headers: { origin: ORIGIN, cookie: legacy, 'content-type': 'application/json', [h.ipHeader]: freshIp() }, body: '{}' });
    assert.equal(out.status, 200, await out.clone().text());
    const cleared = out.headers.getSetCookie();
    assert.ok(cleared.some((c) => c.startsWith(`${LEGACY_COOKIE}=;`) && /max-age=0/i.test(c)), cleared.join(' | '));
    assert.equal((await internalSession(h, legacy)).status, 401);
    // A new name wins over a stale old one.
    await h.resetLimits();
    const fresh = await signInByEmail(h, email);
    const both = `${LEGACY_COOKIE}=stale.value; ${fresh.cookie}`;
    assert.equal((await internalSession(h, both)).status, 200);
  });

  test('a mailbox owner on an address that signed in before is not locked out', async () => {
    await h.resetLimits();
    // Gate fix 7: anyone may request codes for an address (5 an hour, one a
    // minute). Requests from a client address that already proved this mailbox
    // count against a separate allowance, so a stranger cannot use them up.
    const email = 'owner-lockout@example.test';
    h.canRegister.state.allowed.add(email);
    const { response, ip: home } = await signInByEmail(h, email);
    assert.equal(response.status, 200);
    const stranger = await sendCode(h, email, freshIp());
    assert.equal(stranger.status, 429, 'the minute gap since the owner\'s code');
    await h.resetLimits();
    assert.equal((await sendCode(h, email, freshIp())).status, 200, 'a stranger asks for a code');
    assert.equal((await sendCode(h, email, freshIp())).status, 429, 'strangers share one allowance');
    const owner = await sendCode(h, email, home);
    assert.equal(owner.status, 200, `the owner at home is not locked out: ${await owner.clone().text()}`);
    assert.equal((await sendCode(h, email, home)).status, 429, 'the owner has a minute gap too');
  });

  test('/internal/session needs the secret and returns the session', async () => {
    const email = 'internal@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    assert.equal((await internalSession(h, cookie, null)).status, 401);
    const wrongSecret = await internalSession(h, cookie, `${INTERNAL_SECRET}x`);
    assert.equal(wrongSecret.status, 401);
    assert.equal((await wrongSecret.json()).code, 'auth.internal_unauthorized');
    assert.equal((await h.request('/internal/health')).status, 401);
    const ok = await internalSession(h, cookie);
    assert.equal(ok.status, 200);
    const body = await ok.json();
    assert.deepEqual(Object.keys(body).sort(), ['auth_user_id', 'email', 'email_verified', 'expires_at', 'mfa_enrolled', 'mfa_method', 'mfa_verified_at', 'session_id']);
    // UPD1 review #8: every recorded version of this answer (api/internal/)
    // is still met, field by field and type by type: the api Worker of the
    // previous release reads it during an update.
    for (const name of readdirSync(CONTRACT).filter((n) => /^auth-session\.v\d+\.json$/.test(n))) {
      const recorded = JSON.parse(readFileSync(join(CONTRACT, name), 'utf8'));
      for (const [key, value] of Object.entries(recorded)) assert.equal(typeof body[key], typeof value, `${name}: ${key}`);
    }
    assert.equal(body.mfa_enrolled, false);
    assert.equal(body.mfa_method, null);
    assert.equal(body.mfa_verified_at, null);
    assert.equal(body.email, email);
    assert.equal(body.email_verified, true);
    assert.equal(typeof body.expires_at, 'number');
    const none = await internalSession(h, 'runbook.session_token=forged.value');
    assert.equal(none.status, 401);
    assert.equal((await none.json()).code, 'auth.no_session');
    // Sign-out ends it.
    const out = await h.request('/api/auth/sign-out', { method: 'POST', headers: { origin: ORIGIN, cookie, 'content-type': 'application/json', [h.ipHeader]: freshIp() }, body: '{}' });
    assert.equal(out.status, 200, await out.clone().text());
    assert.equal((await internalSession(h, cookie)).status, 401);
    const health = await h.request('/internal/health', { headers: { 'x-runbook-internal-secret': INTERNAL_SECRET } });
    assert.deepEqual(await health.json(), { status: 'ok', methods: ['email', 'google'] });
  });

  test('a can-register refusal leaves no user row', async () => {
    const email = 'stranger@example.test';
    const calls = h.canRegister.state.calls.length;
    const ip = freshIp();
    assert.equal((await sendCode(h, email, ip)).status, 200);
    const r = await post(h, '/sign-in/email-otp', { email, otp: latestCode(h, email) }, { ip });
    assert.equal(r.status, 403);
    assert.equal((await r.json()).code, 'auth.registration_refused');
    assert.equal(sessionSetCookie(r), undefined);
    assert.equal(await h.count('user', email), 0);
    assert.equal(await h.count('session', email), 0);
    assert.deepEqual(h.canRegister.state.calls.slice(calls), [{ method: 'POST', path: '/internal/auth/can-register', secret: INTERNAL_SECRET, email }]);
    // Fails closed when Rust errors.
    const flaky = 'flaky@example.test';
    h.canRegister.state.allowed.add(flaky);
    h.canRegister.state.status = 500;
    try {
      const ip2 = freshIp();
      assert.equal((await sendCode(h, flaky, ip2)).status, 200);
      assert.equal((await post(h, '/sign-in/email-otp', { email: flaky, otp: latestCode(h, flaky) }, { ip: ip2 })).status, 403);
      assert.equal(await h.count('user', flaky), 0);
    } finally {
      h.canRegister.state.status = 200;
    }
  });

  test('unsafe auth requests need the trusted origin', async () => {
    for (const origin of [null, 'https://evil.example.test', 'http://runbook.example.test']) {
      const r = await post(h, '/email-otp/send-verification-otp', { email: 'origin@example.test', type: 'sign-in' }, { origin });
      assert.equal(r.status, 403, String(origin));
      assert.equal((await r.json()).code, 'auth.csrf');
    }
    assert.equal(h.mail().filter((m) => m.to === 'origin@example.test').length, 0);
  });

  test('send limits: 60 s gap per email and 10 an hour per IP', async () => {
    await h.resetLimits();
    const ip = freshIp();
    assert.equal((await sendCode(h, 'gap@example.test', ip)).status, 200);
    const again = await sendCode(h, 'gap@example.test', freshIp());
    assert.equal(again.status, 429);
    assert.equal(again.headers.get('retry-after'), '60');
    const shared = freshIp();
    for (let i = 0; i < 10; i++) assert.equal((await sendCode(h, `ip${i}@example.test`, shared)).status, 200, `send ${i}`);
    const eleventh = await sendCode(h, 'ip10@example.test', shared);
    assert.equal(eleventh.status, 429);
    assert.equal(eleventh.headers.get('retry-after'), '3600');
  });

  test('only the host-set client-address header keys the per-IP limit', async () => {
    await h.resetLimits();
    // Gate fix 1: whatever else the client claims, one address is one bucket.
    const other = h.ipHeader === 'cf-connecting-ip' ? 'x-runbook-client-ip' : 'cf-connecting-ip';
    const shared = freshIp();
    for (let i = 0; i < 10; i++) {
      const spoof = `203.0.113.${i + 1}`;
      const headers = { 'x-forwarded-for': spoof, 'x-real-ip': spoof, forwarded: `for=${spoof}`, [other]: spoof, 'true-client-ip': spoof };
      assert.equal((await sendCode(h, `spoof${i}@example.test`, shared, headers)).status, 200, `send ${i}`);
    }
    const limited = await sendCode(h, 'spoof10@example.test', shared, { 'x-forwarded-for': '203.0.113.99' });
    assert.equal(limited.status, 429);
    assert.equal(limited.headers.get('retry-after'), '3600', 'the hourly per-IP limit, not Better Auth\'s own');
    // An IPv6 client holds a whole /64: it is one bucket (as Better Auth's own limiter keys it).
    for (let i = 0; i < 10; i++) assert.equal((await sendCode(h, `six${i}@example.test`, `2001:db8:77:1::${i + 1}`)).status, 200, `v6 send ${i}`);
    const six = await sendCode(h, 'six10@example.test', '2001:db8:77:1:ffff::9');
    assert.equal(six.status, 429);
    assert.equal(six.headers.get('retry-after'), '3600', 'the hourly per-IP limit, not Better Auth\'s own');
    assert.equal((await sendCode(h, 'six11@example.test', '2001:db8:77:2::1')).status, 200);
  });

  test('every answer proves the per-boot secret for the host\'s nonce', async () => {
    // Gate fix 2: the Rust host refuses an answer without it (a port squatter).
    const nonce = '0123456789abcdef0123456789abcdef';
    const expected = createHmac('sha256', INTERNAL_SECRET).update(`runbook-auth-proof\n${nonce}`).digest('hex');
    for (const [path, init] of [
      ['/api/auth/ok', {}],
      ['/api/auth/nope', {}],
      ['/internal/session', { method: 'POST', headers: { 'x-runbook-internal-secret': INTERNAL_SECRET } }],
      ['/internal/session', { method: 'POST' }],
    ]) {
      const r = await h.request(path, { ...init, headers: { ...(init.headers ?? {}), 'x-runbook-auth-nonce': nonce } });
      assert.equal(r.headers.get('x-runbook-auth-proof'), expected, `${path} ${r.status}`);
    }
    assert.equal((await h.request('/api/auth/ok')).headers.get('x-runbook-auth-proof'), null);
    const odd = await h.request('/api/auth/ok', { headers: { 'x-runbook-auth-nonce': 'not hex!' } });
    assert.equal(odd.headers.get('x-runbook-auth-proof'), null);
  });

  test('a failed delivery voids the code', async () => {
    const email = 'fail@example.test';
    const r = await sendCode(h, email);
    assert.equal(r.status, 503);
    assert.equal((await r.json()).code, 'auth.email_unavailable');
    assert.equal(await h.count('verification', email), 0);
  });

  test('no request left the machine', () => {
    assert.deepEqual(h.outbound.unexpected, []);
  });
}
