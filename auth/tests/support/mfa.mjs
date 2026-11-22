import assert from 'node:assert/strict';
import { createHash, createHmac, generateKeyPairSync, randomBytes, sign } from 'node:crypto';
import { test } from 'node:test';
import { encodeCBOR } from '@levischuck/tiny-cbor';
import { authorizeUrl, consentFlow, exchange, introspect, pkce, register, tokenRequest } from './oauth.mjs';
import { cookieHeader, post, signInByEmail } from './suite.mjs';
import { INTERNAL_SECRET, ORIGIN } from './stubs.mjs';

const base64 = (bytes) => Buffer.from(bytes).toString('base64url');
const hash = (bytes) => createHash('sha256').update(bytes).digest();
const rpHash = hash(new URL(ORIGIN).hostname);
const json = async (response) => {
  const text = await response.text();
  assert.equal(response.status, 200, text);
  return JSON.parse(text);
};
const sessionCookie = (resp) => cookieHeader(resp).split('; ').filter((c) => c.startsWith('__Host-runbook.session_token=')).at(-1);
const readSession = (h, cookie) => h.request('/internal/session', { method: 'POST', headers: { cookie, 'x-runbook-internal-secret': INTERNAL_SECRET } });
const get = (h, path, cookie) => h.request(`/api/auth${path}`, { headers: cookie ? { cookie } : {} });

export function totp(uri, skew = 0) {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  const secret = new URL(uri).searchParams.get('secret');
  const bits = Array.from(secret, (c) => alphabet.indexOf(c).toString(2).padStart(5, '0')).join('');
  const key = Buffer.from(bits.match(/.{8}/g).map((byte) => parseInt(byte, 2)));
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(Math.floor(Date.now() / 30_000) + skew));
  const digest = createHmac('sha1', key).update(counter).digest();
  return String((digest.readUInt32BE(digest.at(-1) & 15) & 0x7fffffff) % 1_000_000).padStart(6, '0');
}

function authenticator() {
  const { publicKey, privateKey } = generateKeyPairSync('ec', { namedCurve: 'prime256v1' });
  const { x, y } = publicKey.export({ format: 'jwk' });
  const credential = randomBytes(32);
  const id = base64(credential);
  const cose = encodeCBOR(new Map([[1, 2], [3, -7], [-1, 1], [-2, Buffer.from(x, 'base64url')], [-3, Buffer.from(y, 'base64url')]]));
  const clientData = (type, challenge) => Buffer.from(JSON.stringify({ type, challenge, origin: ORIGIN }));
  let count = 0;
  function authData(flags) {
    const counter = Buffer.alloc(4);
    counter.writeUInt32BE(count);
    return Buffer.concat([rpHash, Buffer.of(flags), counter]);
  }
  return {
    register(challenge) {
      const attested = Buffer.concat([authData(0x45), Buffer.alloc(16), Buffer.of(0, credential.length), credential, cose]);
      return { id, rawId: id, type: 'public-key', response: {
        clientDataJSON: base64(clientData('webauthn.create', challenge)),
        attestationObject: base64(encodeCBOR(new Map([['fmt', 'none'], ['attStmt', new Map()], ['authData', attested]]))),
      } };
    },
    assert(challenge, uv = true) {
      count += 1;
      const client = clientData('webauthn.get', challenge);
      const data = authData(uv ? 5 : 1);
      return { id, rawId: id, type: 'public-key', response: {
        clientDataJSON: base64(client), authenticatorData: base64(data),
        signature: base64(sign('sha256', Buffer.concat([data, hash(client)]), privateKey)),
      } };
    },
  };
}

export function mfaSuite(h) {
  test('MFA: authenticator setup confirmation never upgrades the session', async () => {
    const email = 'mfa-confirmation@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const setup = await json(await post(h, '/two-factor/enable', {}, { cookie }));
    const confirmation = await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI) }, { cookie });
    assert.deepEqual(await json(confirmation), { status: true, enrolled: true });
    assert.equal(sessionCookie(confirmation), undefined);
    const session = await json(await readSession(h, cookie));
    assert.equal(session.mfa_enrolled, true);
    assert.equal(session.mfa_verified_at, null);
  });

  test('MFA: pending setup expires and belongs only to its original session', async () => {
    const email = 'mfa-pending-session@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const { session_id: id } = await json(await readSession(h, cookie));
    const setup = await json(await post(h, '/two-factor/enable', {}, { cookie }));
    await h.resetLimits();
    const { cookie: other } = await signInByEmail(h, email);
    assert.equal((await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI) }, { cookie: other })).status, 400);
    h.canRegister.state.security = { mfa_required: true, mfa_partial: true };
    try {
      const start = await post(h, '/two-factor/enable', {}, { cookie: other });
      assert.equal(start.status, 403);
      assert.equal((await start.json()).code, 'auth.mfa_step_up');
      const finish = await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI) }, { cookie });
      assert.equal(finish.status, 403);
    } finally {
      delete h.canRegister.state.security;
    }
    await h.setTotpSetupExpiry(id, Date.now() - 1000);
    const expired = await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI) }, { cookie });
    assert.ok(expired.status >= 400);
    assert.equal((await json(await readSession(h, cookie))).mfa_enrolled, false);
  });

  test('MFA: a pending authenticator cannot verify an account now protected by a passkey', async () => {
    const email = 'mfa-pending-with-passkey@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie: setupCookie } = await signInByEmail(h, email);
    const setup = await json(await post(h, '/two-factor/enable', {}, { cookie: setupCookie }));
    await h.resetLimits();
    const { cookie } = await signInByEmail(h, email);
    const registration = await get(h, '/passkey/generate-register-options', cookie);
    const registrationCookie = cookieHeader(registration);
    const { challenge } = await json(registration);
    await json(await post(h, '/passkey/verify-registration', { response: authenticator().register(challenge) }, {
      cookie: `${cookie}; ${registrationCookie}`,
    }));
    const abandoned = await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI) }, { cookie: setupCookie });
    assert.equal(abandoned.status, 400);
    assert.equal(sessionCookie(abandoned), undefined);
    await h.resetLimits();
    const { cookie: recent } = await signInByEmail(h, email);
    const code = await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI) }, { cookie: recent });
    assert.equal(code.status, 400);
    assert.equal(sessionCookie(code), undefined);
    const backup = await post(h, '/two-factor/verify-backup-code', { code: setup.backupCodes[0] }, { cookie: recent });
    assert.equal(backup.status, 400);
    const state = await json(await readSession(h, recent));
    assert.equal(state.mfa_enrolled, true);
    assert.equal(state.mfa_verified_at, null);
  });

  test('MFA: authenticator enrollment, skew, backup consumption and factor/IP lockout', async () => {
    const email = 'mfa-totp@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const setup = await json(await post(h, '/two-factor/enable', {}, { cookie }));
    assert.equal((await json(await readSession(h, cookie))).mfa_enrolled, false);
    const confirmation = await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI, 1) }, { cookie });
    assert.deepEqual(await json(confirmation), { status: true, enrolled: true });
    assert.equal(setup.backupCodes.length, 10);
    const verify = await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI, -1) }, { cookie });
    const verifiedCookie = sessionCookie(verify);
    assert.ok(verifiedCookie);
    assert.deepEqual(await json(verify), { status: true });
    assert.equal((await readSession(h, cookie)).status, 401);
    const verified = await json(await readSession(h, verifiedCookie));
    assert.equal(verified.mfa_method, 'totp');
    assert.equal(verified.mfa_enrolled, true);
    assert.ok(verified.mfa_verified_at > Date.now() - 10_000);
    const backup = await post(h, '/two-factor/verify-backup-code', { code: setup.backupCodes[0] }, { cookie: verifiedCookie });
    const backupCookie = sessionCookie(backup);
    assert.deepEqual(await json(backup), { status: true });
    assert.equal((await json(await readSession(h, backupCookie))).mfa_method, 'backup_code');
    await h.resetLimits();
    const { cookie: firstFactorOnly } = await signInByEmail(h, email);
    assert.equal((await json(await readSession(h, firstFactorOnly))).mfa_verified_at, null);
    const limitedIp = '192.0.2.80';
    for (let i = 0; i < 5; i++) {
      const reused = await post(h, '/two-factor/verify-backup-code', { code: setup.backupCodes[0] }, { cookie: firstFactorOnly, ip: limitedIp });
      assert.equal(reused.status, 401);
    }
    const locked = await post(h, '/two-factor/verify-backup-code', { code: setup.backupCodes[1] }, { cookie: firstFactorOnly, ip: limitedIp });
    assert.equal(locked.status, 429);
    assert.equal((await locked.json()).code, 'auth.mfa_locked');
    const separateAddress = await post(h, '/two-factor/verify-backup-code', { code: setup.backupCodes[1] }, { cookie: backupCookie, ip: '192.0.2.81' });
    await json(separateAddress);
    const currentCode = totp(setup.totpURI);
    const otherFactor = await post(h, '/two-factor/verify-totp', { code: currentCode }, { cookie: sessionCookie(separateAddress), ip: limitedIp });
    await json(otherFactor);
    for (let i = 0; i < 4; i++) {
      const reused = await post(h, '/two-factor/verify-backup-code', { code: setup.backupCodes[0] }, {
        cookie: firstFactorOnly, ip: `192.0.2.${90 + i}`,
      });
      assert.equal(reused.status, 401);
    }
    for (const ip of ['192.0.2.94', '192.0.2.95']) {
      const capped = await post(h, '/two-factor/verify-backup-code', { code: setup.backupCodes[0] }, {
        cookie: firstFactorOnly, ip,
      });
      assert.equal(capped.status, 429);
    }
    for (let i = 0; i < 10; i++) {
      const reused = await post(h, '/two-factor/verify-totp', { code: currentCode }, {
        cookie: firstFactorOnly, ip: `192.0.2.${100 + i}`,
      });
      assert.equal(reused.status, i === 9 ? 429 : 401);
    }
    const { auth_user_id } = await json(await readSession(h, firstFactorOnly));
    assert.equal((await h.mfaAudit(auth_user_id)).filter(({ action }) => action === 'mfa.locked').length, 3);

    const freshCookie = sessionCookie(otherFactor);
    const device = authenticator();
    const registration = await get(h, '/passkey/generate-register-options', freshCookie);
    const { challenge: registrationChallenge } = await json(registration);
    await json(await post(h, '/passkey/verify-registration', { response: device.register(registrationChallenge) }, {
      cookie: `${freshCookie}; ${cookieHeader(registration)}`,
    }));
    const options = await get(h, '/passkey/generate-authenticate-options');
    const assertion = device.assert((await json(options)).challenge);
    const signedIn = await post(h, '/passkey/verify-authentication', { response: assertion }, {
      cookie: cookieHeader(options), ip: limitedIp,
    });
    assert.equal((await json(await readSession(h, sessionCookie(signedIn)))).mfa_method, 'passkey');
  });

  test('MFA: authenticator proofs cannot authorize changes under passkey-only policy', async () => {
    const email = 'mfa-passkey-only@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const setup = await json(await post(h, '/two-factor/enable', {}, { cookie }));
    const confirmation = await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI, 1) }, { cookie });
    assert.deepEqual(await json(confirmation), { status: true, enrolled: true });
    const verified = sessionCookie(await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI) }, { cookie }));
    assert.ok(verified);
    h.canRegister.state.security = { allow_authenticator: false };
    try {
      const options = await get(h, '/passkey/generate-register-options', verified);
      assert.equal((await options.json()).code, 'auth.mfa_step_up');
      const totpCheck = await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI) }, { cookie: verified });
      assert.equal(totpCheck.status, 403);
      assert.equal((await totpCheck.json()).code, 'auth.mfa_factor_disallowed');
      assert.equal(sessionCookie(totpCheck), undefined);
      const backup = await post(h, '/two-factor/verify-backup-code', { code: setup.backupCodes[0] }, { cookie: verified });
      assert.equal((await backup.json()).code, 'auth.mfa_factor_disallowed');
    } finally {
      delete h.canRegister.state.security;
    }
  });

  test('MFA: policy protects the last factor and OAuth preserves the authorization check', async () => {
    const email = 'mfa-policy@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const setup = await json(await post(h, '/two-factor/enable', {}, { cookie }));
    const confirmation = await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI, 1) }, { cookie });
    assert.deepEqual(await json(confirmation), { status: true, enrolled: true });
    const client = await register(h);
    const pk = pkce();
    const authorize = authorizeUrl(client, { challenge: pk.challenge, scope: 'runbooks:read offline_access' });
    h.canRegister.state.security = { mfa_required: true, mfa_partial: true };
    try {
      const blocked = await h.request(authorize, { headers: { cookie } });
      assert.equal(blocked.status, 303);
      const redirect = new URL(blocked.headers.get('location'), ORIGIN);
      assert.equal(redirect.pathname, '/settings/security');
      assert.equal(redirect.searchParams.get('return_to'), authorize);
      const code = totp(setup.totpURI);
      const verify = await post(h, '/two-factor/verify-totp', { code }, { cookie });
      const verified = sessionCookie(verify);
      assert.deepEqual(await json(verify), { status: true });
      const reused = await post(h, '/two-factor/verify-totp', { code }, { cookie: verified });
      assert.equal(reused.status, 401);
      assert.equal((await reused.json()).code, 'auth.mfa_code_reused');
      const last = await post(h, '/two-factor/disable', {}, { cookie: verified });
      assert.equal(last.status, 409);
      h.canRegister.state.security.mfa_partial = false;
      const flow = await consentFlow(h, { client, pk, cookie: verified, email, scope: 'runbooks:read offline_access' });
      const issued = await json(await exchange(h, flow));
      const initial = await introspect(h, issued.access_token);
      assert.equal(initial.body.mfa_method, 'totp');
      assert.ok(initial.body.mfa_verified_at > Date.now() - 10_000);
      assert.equal((await post(h, '/sign-out', {}, { cookie: verified })).status, 200);
      const refreshed = await json(await tokenRequest(h, {
        grant_type: 'refresh_token', client_id: client.client_id, refresh_token: issued.refresh_token,
      }));
      const next = await introspect(h, refreshed.access_token);
      assert.equal(next.body.mfa_verified_at, initial.body.mfa_verified_at);
      const reset = await h.request('/internal/mfa/reset', {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-runbook-internal-secret': INTERNAL_SECRET },
        body: JSON.stringify({ auth_user_ids: [next.body.auth_user_id], actor: 'test-owner' }),
      });
      assert.deepEqual(await json(reset), { reset: true });
      assert.equal((await introspect(h, refreshed.access_token)).status, 401);
    } finally {
      delete h.canRegister.state.security;
    }
  });

  test('MFA: removing a factor clears its standing on existing and refreshed grants', async () => {
    const email = 'mfa-grant-removal@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const setup = await json(await post(h, '/two-factor/enable', {}, { cookie }));
    await json(await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI, 1) }, { cookie }));
    const verified = sessionCookie(await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI) }, { cookie }));
    assert.ok(verified);
    const client = await register(h);
    const pk = pkce();
    const flow = await consentFlow(h, { client, pk, cookie: verified, email, scope: 'runbooks:read offline_access' });
    const issued = await json(await exchange(h, flow));
    assert.equal((await introspect(h, issued.access_token)).body.mfa_method, 'totp');
    await json(await post(h, '/two-factor/disable', {}, { cookie: verified }));
    assert.equal((await introspect(h, issued.access_token)).body.mfa_method, null);
    const refreshed = await json(await tokenRequest(h, {
      grant_type: 'refresh_token', client_id: client.client_id, refresh_token: issued.refresh_token,
    }));
    const facts = (await introspect(h, refreshed.access_token)).body;
    assert.equal(facts.mfa_enrolled, false);
    assert.equal(facts.mfa_verified_at, null);
  });

  test('MFA: an enrolled account requires a verified session for session management', async () => {
    const email = 'mfa-session-management@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const device = authenticator();
    const challenge = await get(h, '/passkey/generate-register-options', cookie);
    const challengeCookie = cookieHeader(challenge);
    const { challenge: value } = await json(challenge);
    const enrolled = await post(h, '/passkey/verify-registration', {
      response: device.register(value),
    }, { cookie: `${cookie}; ${challengeCookie}` });
    const verified = sessionCookie(enrolled);
    await json(enrolled);
    await h.resetLimits();
    const { cookie: unverified } = await signInByEmail(h, email);
    assert.equal((await get(h, '/list-sessions', unverified)).status, 403);
    assert.equal((await get(h, '/passkey/list-user-passkeys', unverified)).status, 403);
    assert.equal((await get(h, '/passkey/list-user-passkeys', verified)).status, 200);
    assert.equal((await post(h, '/revoke-other-sessions', {}, { cookie: unverified })).status, 403);
    const sessionId = (await json(await readSession(h, unverified))).session_id;
    const sessions = await json(await get(h, '/list-sessions', verified));
    assert.ok(sessions.some((session) => session.id === sessionId));
    assert.ok(sessions.every((session) => !Object.hasOwn(session, 'token')));
    await json(await post(h, '/revoke-session', { session_id: sessionId }, { cookie: verified }));
    assert.equal((await readSession(h, unverified)).status, 401);
  });

  test('MFA: passkey enrollment asks for a new sign-in on an old session', async () => {
    const email = 'mfa-old-session@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const { session_id: id } = await json(await readSession(h, cookie));
    await h.setSessionCreatedAt(id, Date.now() - 25 * 3_600_000);
    const response = await get(h, '/passkey/generate-register-options', cookie);
    assert.equal(response.status, 403);
    assert.equal((await response.json()).code, 'auth.sign_in_again');
  });

  test('MFA: reset invalidates outstanding passkey registrations', async () => {
    const email = 'mfa-reset-registration@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const { auth_user_id: id } = await json(await readSession(h, cookie));
    const options = await get(h, '/passkey/generate-register-options', cookie);
    const challengeCookie = cookieHeader(options);
    const { challenge } = await json(options);
    const reset = await h.request('/internal/mfa/reset', {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-runbook-internal-secret': INTERNAL_SECRET },
      body: JSON.stringify({ auth_user_ids: [id], actor: 'test-owner' }),
    });
    assert.deepEqual(await json(reset), { reset: true });
    await h.resetLimits();
    const { cookie: next } = await signInByEmail(h, email);
    const response = await post(h, '/passkey/verify-registration', { response: authenticator().register(challenge) }, {
      cookie: `${next}; ${challengeCookie}`,
    });
    assert.equal(response.status, 403);
    assert.equal((await response.json()).code, 'auth.mfa_setup_changed');
    assert.equal((await json(await readSession(h, next))).mfa_enrolled, false);
  });

  test('MFA: reset fences an assertion already verified by the authenticator', { timeout: 10_000 }, async () => {
    const email = 'mfa-reset-assertion@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const { auth_user_id: id } = await json(await readSession(h, cookie));
    const device = authenticator();
    const registration = await get(h, '/passkey/generate-register-options', cookie);
    const registrationCookie = cookieHeader(registration);
    const { challenge } = await json(registration);
    await json(await post(h, '/passkey/verify-registration', { response: device.register(challenge) }, {
      cookie: `${cookie}; ${registrationCookie}`,
    }));

    const options = await get(h, '/passkey/generate-authenticate-options');
    const assertionCookie = cookieHeader(options);
    const assertion = device.assert((await json(options)).challenge);
    const atPolicy = Promise.withResolvers();
    const afterReset = Promise.withResolvers();
    h.canRegister.state.beforeMfaPolicy = async () => {
      atPolicy.resolve();
      await afterReset.promise;
    };
    const verifying = post(h, '/passkey/verify-authentication', { response: assertion }, { cookie: assertionCookie });
    try {
      await Promise.race([atPolicy.promise, verifying.then(() => assert.fail('The proof never reached the policy barrier'))]);
      const reset = await h.request('/internal/mfa/reset', {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-runbook-internal-secret': INTERNAL_SECRET },
        body: JSON.stringify({ auth_user_ids: [id], actor: 'test-owner' }),
      });
      assert.deepEqual(await json(reset), { reset: true });
    } finally {
      delete h.canRegister.state.beforeMfaPolicy;
      afterReset.resolve();
    }
    const refused = await verifying;
    assert.equal(refused.status, 401);
    assert.equal(sessionCookie(refused), undefined);
    assert.equal(await h.count('session', email), 0);
  });

  test('MFA: factor removal fences an assertion already verified by the authenticator', { timeout: 10_000 }, async () => {
    const email = 'mfa-removal-assertion@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const device = authenticator();
    const registration = await get(h, '/passkey/generate-register-options', cookie);
    const registrationCookie = cookieHeader(registration);
    const { challenge } = await json(registration);
    const enrolled = await post(h, '/passkey/verify-registration', { response: device.register(challenge) }, {
      cookie: `${cookie}; ${registrationCookie}`,
    });
    const verified = sessionCookie(enrolled);
    await json(enrolled);
    const [passkey] = await json(await get(h, '/passkey/list-user-passkeys', verified));
    const before = await h.count('session', email);

    const options = await get(h, '/passkey/generate-authenticate-options');
    const assertionCookie = cookieHeader(options);
    const assertion = device.assert((await json(options)).challenge);
    const atPolicy = Promise.withResolvers();
    const afterReset = Promise.withResolvers();
    h.canRegister.state.beforeMfaPolicy = async () => {
      atPolicy.resolve();
      await afterReset.promise;
    };
    const verifying = post(h, '/passkey/verify-authentication', { response: assertion }, { cookie: assertionCookie });
    try {
      await Promise.race([atPolicy.promise, verifying.then(() => assert.fail('The proof never reached the policy barrier'))]);
      delete h.canRegister.state.beforeMfaPolicy;
      await json(await post(h, '/passkey/delete-passkey', { id: passkey.id }, { cookie: verified }));
    } finally {
      delete h.canRegister.state.beforeMfaPolicy;
      afterReset.resolve();
    }
    const refused = await verifying;
    assert.equal(refused.status, 401);
    assert.equal(sessionCookie(refused), undefined);
    assert.equal(await h.count('session', email), before);
  });

  test('MFA: setup start, backup use and regeneration each queue distinct security mail', async () => {
    const email = 'mfa-audit-recovery@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const { auth_user_id: id } = await json(await readSession(h, cookie));
    const setup = await json(await post(h, '/two-factor/enable', {}, { cookie }));
    await json(await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI, 1) }, { cookie }));
    const used = await post(h, '/two-factor/verify-backup-code', { code: setup.backupCodes[0] }, { cookie });
    const verified = sessionCookie(used);
    await json(used);
    await json(await post(h, '/two-factor/generate-backup-codes', {}, { cookie: verified }));
    const audit = new Map((await h.mfaAudit(id)).map((row) => [row.action, row]));
    const actions = {
      'mfa.totp_setup_started': 'started',
      'mfa.backup_code_used': 'used',
      'mfa.backup_codes_regenerated': 'regenerated',
    };
    for (const [action, notification] of Object.entries(actions)) {
      assert.equal(audit.get(action)?.notification, notification, action);
      assert.ok(audit.get(action)?.notified_at, action);
    }
  });

  test('MFA: fail an enrollment or removal atomically when its audit cannot be written', async () => {
    const email = 'mfa-audit-atomic@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const device = authenticator();
    async function enroll() {
      const registration = await get(h, '/passkey/generate-register-options', cookie);
      const registrationCookie = cookieHeader(registration);
      const { challenge } = await json(registration);
      return post(h, '/passkey/verify-registration', { response: device.register(challenge) }, {
        cookie: `${cookie}; ${registrationCookie}`,
      });
    }
    await h.rejectMfaAudit();
    try {
      assert.ok((await enroll()).status >= 400);
      assert.equal((await json(await readSession(h, cookie))).mfa_enrolled, false);
    } finally {
      await h.restoreMfaAudit();
    }
    const enrolled = await enroll();
    const verified = sessionCookie(enrolled);
    await json(enrolled);
    const [passkey] = await json(await get(h, '/passkey/list-user-passkeys', verified));
    await h.rejectMfaAudit();
    try {
      assert.ok((await post(h, '/passkey/delete-passkey', { id: passkey.id }, { cookie: verified })).status >= 400);
      assert.equal((await json(await readSession(h, verified))).mfa_enrolled, true);
    } finally {
      await h.restoreMfaAudit();
    }
  });

  test('MFA: public assertion failures do not disclose credential existence', async () => {
    const email = 'mfa-assertion-errors@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const device = authenticator();
    const registration = await get(h, '/passkey/generate-register-options', cookie);
    const challengeCookie = cookieHeader(registration);
    const { challenge } = await json(registration);
    await json(await post(h, '/passkey/verify-registration', {
      response: device.register(challenge),
    }, { cookie: `${cookie}; ${challengeCookie}` }));
    async function failAssertion(candidate, invalidateSignature) {
      const options = await get(h, '/passkey/generate-authenticate-options');
      const cookie = cookieHeader(options);
      const assertion = candidate.assert((await json(options)).challenge);
      if (invalidateSignature) {
        const signature = Buffer.from(assertion.response.signature, 'base64url');
        signature[signature.length - 1] ^= 1;
        assertion.response.signature = base64(signature);
      }
      const response = await post(h, '/passkey/verify-authentication', { response: assertion }, { cookie });
      return { status: response.status, body: await response.json() };
    }
    const unknown = await failAssertion(authenticator(), false);
    const invalid = await failAssertion(device, true);
    assert.deepEqual(unknown, invalid);
  });

  test('MFA: software passkey registration and user-verified sign-in', async () => {
    const email = 'mfa-passkey@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const device = authenticator();
    const reg = await get(h, '/passkey/generate-register-options', cookie);
    const regCookie = `${cookie}; ${cookieHeader(reg)}`;
    const options = await json(reg);
    assert.equal(options.authenticatorSelection.userVerification, 'required');
    const registered = await post(h, '/passkey/verify-registration', { response: device.register(options.challenge), name: 'Software authenticator' }, { cookie: regCookie });
    const enrolledCookie = sessionCookie(registered);
    assert.deepEqual(await json(registered), { status: true });
    const registeredSession = await json(await readSession(h, enrolledCookie));
    assert.equal(registeredSession.mfa_enrolled, true);
    assert.equal(registeredSession.mfa_method, 'passkey');
    const makeAssertion = async (uv) => {
      const response = await get(h, '/passkey/generate-authenticate-options');
      const challengeCookie = cookieHeader(response);
      const assertionOptions = await json(response);
      assert.equal(assertionOptions.userVerification, 'required');
      return { challengeCookie, body: { response: device.assert(assertionOptions.challenge, uv) } };
    };
    const noUv = await makeAssertion(false);
    const refused = await post(h, '/passkey/verify-authentication', noUv.body, { cookie: noUv.challengeCookie });
    assert.equal(refused.status, 403);
    assert.equal((await refused.json()).code, 'auth.passkey_uv_required');
    const uv = await makeAssertion(true);
    const authenticated = await post(h, '/passkey/verify-authentication', uv.body, { cookie: uv.challengeCookie });
    const signedIn = sessionCookie(authenticated);
    await json(authenticated);
    const verified = await json(await readSession(h, signedIn));
    assert.equal(verified.mfa_method, 'passkey');
    assert.equal(verified.mfa_enrolled, true);
    assert.ok(verified.mfa_verified_at > Date.now() - 10_000);
    assert.equal((await post(h, '/passkey/verify-authentication', uv.body, { cookie: uv.challengeCookie })).status, 400);
  });
  test('MFA: the factors report counts enrolled passkeys and verified authenticators only', async () => {
    const factors = async (ids) => {
      const response = await h.request('/internal/mfa/factors', {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-runbook-internal-secret': INTERNAL_SECRET },
        body: JSON.stringify({ auth_user_ids: ids }),
      });
      return json(response);
    };
    const appEmail = 'mfa-factors-app@example.test';
    const keyEmail = 'mfa-factors-key@example.test';
    h.canRegister.state.allowed.add(appEmail);
    h.canRegister.state.allowed.add(keyEmail);
    const app = await signInByEmail(h, appEmail);
    const appId = (await json(await readSession(h, app.cookie))).auth_user_id;
    const setup = await json(await post(h, '/two-factor/enable', {}, { cookie: app.cookie }));
    assert.deepEqual((await factors([appId, 'unknown-user'])).factors, [
      { auth_user_id: appId, email: appEmail, passkeys: 0, authenticator: false },
    ]);
    await json(await post(h, '/two-factor/verify-totp', { code: totp(setup.totpURI) }, { cookie: app.cookie }));
    await h.resetLimits();
    const key = await signInByEmail(h, keyEmail);
    const keyId = (await json(await readSession(h, key.cookie))).auth_user_id;
    const reg = await get(h, '/passkey/generate-register-options', key.cookie);
    const { challenge } = await json(reg);
    await json(await post(h, '/passkey/verify-registration', { response: authenticator().register(challenge) }, {
      cookie: `${key.cookie}; ${cookieHeader(reg)}`,
    }));
    const unknownIds = Array.from({ length: 300 }, (_, index) => `unknown-${index}`);
    const ids = [appId, ...unknownIds, keyId, keyId];
    const report = [...(await factors(ids.slice(0, 256))).factors,
      ...(await factors(ids.slice(256))).factors].sort((a, b) => a.email.localeCompare(b.email));
    assert.deepEqual(report, [
      { auth_user_id: appId, email: appEmail, passkeys: 0, authenticator: true },
      { auth_user_id: keyId, email: keyEmail, passkeys: 1, authenticator: false },
    ]);
    const invalid = await h.request('/internal/mfa/factors', {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-runbook-internal-secret': INTERNAL_SECRET },
      body: JSON.stringify({ auth_user_ids: [] }),
    });
    assert.equal(invalid.status, 400);
  });
}
