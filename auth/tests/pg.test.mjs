// server.ts on Node with Postgres. Runs only when DATABASE_URL names a role that
// may create databases (locally: postgres://runbook_dev@localhost:5432/runbook_dev).
// The harness creates OWNED databases named rb_test_auth_<pid>_<n>, works only in
// them and drops them afterwards; it never touches any other database. Wrap runs
// in `lockf <repo>/../.locks/pg.lock` so at most one Postgres suite runs at a time.
//
// The service runs in this process (dist/server.mjs), so the global fetch is
// replaced for the duration: Google answers from a local key, can-register from a
// loopback stub, and anything else is refused. SMTP goes to a loopback fake.
import { after, before, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import postgres from 'postgres';
import { behaviourSuite, cookieHeader, post, signInByEmail } from './support/suite.mjs';
import { mfaSuite } from './support/mfa.mjs';
import { oauthSuite } from './support/oauth.mjs';
import { ALLOWED_DOMAINS, AUTH_SECRET, GOOGLE_CLIENT_ID, GOOGLE_CLIENT_SECRET, INTERNAL_SECRET, ORIGIN, googleMock, outboundPolicy, startCanRegister, startSmtp } from './support/stubs.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const adminUrl = process.env.DATABASE_URL;
const realFetch = globalThis.fetch;
let dbCounter = 0;

/** Creates an owned rb_test_auth_<pid>_<n> database; returns its URL and a dropper. */
async function ownedDatabase() {
  const name = `rb_test_auth_${process.pid}_${++dbCounter}`;
  assert.match(name, /^rb_test_auth_\d+_\d+$/);
  const admin = postgres(adminUrl, { max: 1, onnotice: () => {} });
  await admin.unsafe(`CREATE DATABASE "${name}"`);
  await admin.end();
  const url = new URL(adminUrl);
  url.pathname = `/${name}`;
  return {
    name,
    url: url.toString(),
    async drop() {
      const a = postgres(adminUrl, { max: 1, onnotice: () => {} });
      try {
        // A child that is still shutting down may reconnect once; retry briefly.
        for (let attempt = 0; ; attempt++) {
          try {
            await a.unsafe(`DROP DATABASE IF EXISTS "${name}" WITH (FORCE)`);
            break;
          } catch (error) {
            if (attempt >= 10) throw error;
            await new Promise((r) => setTimeout(r, 200));
          }
        }
      } finally {
        await a.end();
      }
    },
  };
}

const baseEnv = (databaseUrl) => ({ DATABASE_URL: databaseUrl, RUNBOOK_AUTH_INTERNAL_SECRET: INTERNAL_SECRET, RUNBOOK_AUTH_PORT: '0' });

describe('server.ts on Node/Postgres', { skip: adminUrl ? false : 'set DATABASE_URL to a role that may CREATE DATABASE' }, () => {
  let server;
  let database;
  let sql;
  let smtp;
  let canRegister;
  let outbound;
  const google = googleMock();
  const h = {
    ipHeader: 'x-runbook-client-ip',
    google,
    get canRegister() {
      return canRegister;
    },
    get outbound() {
      return outbound;
    },
    request: (path, init = {}) => realFetch(`${server.url}${path}`, { redirect: 'manual', ...init }),
    mail: () => smtp.messages,
    async count(table, email) {
      const rows = {
        user: () => sql`SELECT COUNT(*)::int AS n FROM auth."user" WHERE email = ${email}`,
        session: () => sql`SELECT COUNT(*)::int AS n FROM auth.session s JOIN auth."user" u ON u.id = s.user_id WHERE u.email = ${email}`,
        verification: () => sql`SELECT COUNT(*)::int AS n FROM auth.verification WHERE identifier = ${`sign-in-otp-${email}`}`,
      }[table]();
      return (await rows)[0].n;
    },
    async setSessionExpiry(id, ms) {
      await sql`UPDATE auth.session SET expires_at = ${new Date(ms)} WHERE id = ${id}`;
    },
    async mfaAudit(id) {
      return sql`SELECT action, notification, notified_at FROM auth.mfa_audit WHERE user_id = ${id}`;
    },
    async rejectMfaAudit() {
      await sql`CREATE FUNCTION auth.fail_mfa_audit() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN RAISE EXCEPTION 'test audit write failure'; END; $$`;
      await sql`CREATE TRIGGER fail_mfa_audit BEFORE INSERT ON auth.mfa_audit
        FOR EACH ROW EXECUTE FUNCTION auth.fail_mfa_audit()`;
    },
    async restoreMfaAudit() {
      await sql`DROP TRIGGER fail_mfa_audit ON auth.mfa_audit`;
      await sql`DROP FUNCTION auth.fail_mfa_audit()`;
    },
    async setTotpSetupExpiry(id, ms) {
      await sql`UPDATE auth.verification SET expires_at = ${new Date(ms)} WHERE identifier = ${`totp-setup:${id}`}`;
    },
    async setSessionCreatedAt(id, ms) {
      await sql`UPDATE auth.session SET created_at = ${new Date(ms)} WHERE id = ${id}`;
    },
    async resetLimits() {
      await sql`DELETE FROM auth.send_limit WHERE key NOT LIKE 'known:%'`;
    },
    async verificationValue(email) {
      return (await sql`SELECT value FROM auth.verification WHERE identifier = ${`sign-in-otp-${email}`}`)[0]?.value ?? null;
    },
    async expireAccessTokens() {
      await sql`UPDATE auth.oauth_access_token SET expires_at = ${new Date(Date.now() - 1000)}`;
    },
    async backdateClient(clientId, ms) {
      await sql`UPDATE auth.oauth_client SET created_at = ${new Date(ms)} WHERE client_id = ${clientId}`;
    },
    async backdateConsent(clientId, ms) {
      await sql`UPDATE auth.oauth_consent SET created_at = ${new Date(ms)}, updated_at = ${new Date(ms)} WHERE client_id = ${clientId}`;
    },
    async breakTokens() {
      await sql`ALTER TABLE auth.oauth_access_token RENAME TO oauth_access_token_hidden`;
    },
    async fixTokens() {
      await sql`ALTER TABLE auth.oauth_access_token_hidden RENAME TO oauth_access_token`;
    },
    async deleteClient(clientId) {
      await sql`DELETE FROM auth.oauth_client WHERE client_id = ${clientId}`;
    },
    async deleteUser(email) {
      await sql`DELETE FROM auth."user" WHERE email = ${email}`;
    },
    async clientExists(clientId) {
      return (await sql`SELECT COUNT(*)::int AS n FROM auth.oauth_client WHERE client_id = ${clientId}`)[0].n === 1;
    },
  };

  before(async () => {
    canRegister = await startCanRegister();
    smtp = await startSmtp();
    outbound = outboundPolicy({ google, canRegister, realFetch });
    globalThis.fetch = async (input, init = {}) => {
      const request = new Request(input, init);
      const r = await outbound.route(new URL(request.url), request.method, Object.fromEntries(request.headers), await request.text());
      return new Response(r.body, { status: r.status, headers: r.headers });
    };
    database = await ownedDatabase();
    sql = postgres(database.url, { max: 2, onnotice: () => {} });
    const { startServer } = await import('../dist/server.mjs');
    server = await startServer({
      ...baseEnv(database.url),
      RUNBOOK_AUTH_SECRET: AUTH_SECRET,
      RUNBOOK_ORIGIN: ORIGIN,
      RUNBOOK_AUTH_CAN_REGISTER_URL: canRegister.url,
      RUNBOOK_AUTH_ALLOWED_DOMAINS: ALLOWED_DOMAINS,
      RUNBOOK_SMTP_URL: smtp.url,
      RUNBOOK_SMTP_FROM: 'The Runbook <login@example.test>',
      RUNBOOK_GOOGLE_CLIENT_ID: GOOGLE_CLIENT_ID,
      RUNBOOK_GOOGLE_CLIENT_SECRET: GOOGLE_CLIENT_SECRET,
    });
  });

  after(async () => {
    globalThis.fetch = realFetch;
    await server?.close();
    await sql?.end();
    await database?.drop();
    await smtp?.close();
    await canRegister?.close();
  });

  test('MFA migration accepts legacy writes after reset and fences explicit stale generations', async () => {
    await sql`INSERT INTO auth."user" (id, name, email) VALUES ('legacy-pg-account', 'Legacy', 'legacy-pg@example.test')`;
    await sql`UPDATE auth."user" SET mfa_generation = 1 WHERE id = 'legacy-pg-account'`;
    const [session] = await sql`INSERT INTO auth.session (id, token, user_id, expires_at, updated_at)
      VALUES ('legacy-pg-session', 'legacy-pg-session', 'legacy-pg-account', now() + interval '1 minute', now())
      RETURNING mfa_generation`;
    const [passkey] = await sql`INSERT INTO auth.passkey (id, public_key, user_id, credential_id, counter, device_type, backed_up)
      VALUES ('legacy-pg-passkey', 'pk', 'legacy-pg-account', 'legacy-pg-credential', 0, 'singleDevice', false)
      RETURNING mfa_generation`;
    assert.equal(session.mfa_generation, 1);
    assert.equal(passkey.mfa_generation, 1);
    await assert.rejects(sql`INSERT INTO auth.session (id, token, user_id, expires_at, updated_at, mfa_generation)
      VALUES ('stale-pg-session', 'stale-pg-session', 'legacy-pg-account', now() + interval '1 minute', now(), 0)`, /MFA generation changed/);

    await sql`INSERT INTO auth.oauth_client (id, client_id, redirect_uris) VALUES ('legacy-client', 'legacy-client', '{}')`;
    await sql`INSERT INTO auth.oauth_refresh_token (id, token, client_id, session_id, user_id, scopes)
      VALUES ('legacy-refresh', 'legacy-refresh', 'legacy-client', 'legacy-pg-session', 'legacy-pg-account', '{}')`;
    await sql`INSERT INTO auth.oauth_access_token (id, token, client_id, refresh_id, session_id, user_id, scopes)
      VALUES ('legacy-access', 'legacy-access', 'legacy-client', 'legacy-refresh', 'legacy-pg-session', 'legacy-pg-account', '{}')`;
    const [access] = await sql`SELECT mfa_verified_at, mfa_method FROM auth.oauth_access_token WHERE id = 'legacy-access'`;
    assert.deepEqual(access, { mfa_verified_at: null, mfa_method: null });
  });

  behaviourSuite(h);
  mfaSuite(h);
  oauthSuite(h);

  test('operator reset runs without a listener and records the actor', async () => {
    const email = 'mfa-operator-reset@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const started = await post(h, '/two-factor/enable', {}, { cookie });
    assert.equal(started.status, 200);
    const result = await runProcess(baseEnv(database.url), { untilReady: false, args: ['mfa-reset', email] });
    assert.equal(result.code, 0, result.stderr);
    assert.match(result.stdout, /MFA reset recorded/);
    assert.equal(await h.count('session', email), 0);
    const [{ actor, action }] = await sql`SELECT m.actor, m.action
      FROM auth.mfa_audit m JOIN auth."user" u ON u.id = m.user_id
      WHERE u.email = ${email} AND m.action = 'mfa.reset'`;
    assert.deepEqual({ actor, action }, { actor: 'operator', action: 'mfa.reset' });
  });

  test('tables live in the auth schema only', async () => {
    const outside = await sql`SELECT table_schema, table_name FROM information_schema.tables WHERE table_schema NOT IN ('auth', 'pg_catalog', 'information_schema')`;
    assert.deepEqual([...outside], []);
    const tables = (await sql`SELECT table_name FROM information_schema.tables WHERE table_schema = 'auth' ORDER BY 1`).map((r) => r.table_name);
    assert.deepEqual(tables, ['__drizzle_migrations', 'account', 'mfa_audit', 'oauth_access_token', 'oauth_client', 'oauth_client_assertion', 'oauth_client_resource', 'oauth_consent', 'oauth_refresh_token', 'oauth_resource', 'passkey', 'rate_limit', 'send_limit', 'session', 'two_factor', 'user', 'verification']);
  });
});

/** Runs `node dist/server.mjs` and resolves with its ready line or its exit. */
function runProcess(env, { untilReady = true, args = [] } = {}) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [join(root, 'dist/server.mjs'), ...args], { env: { PATH: process.env.PATH, ...env }, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (c) => {
      stdout += c;
      if (untilReady && stdout.includes('\n')) resolve({ child, ready: JSON.parse(stdout.split('\n')[0]), stderr });
    });
    child.stderr.on('data', (c) => (stderr += c));
    child.on('exit', (code) => resolve({ child, code, stdout, stderr }));
  });
}

const stop = (child) =>
  new Promise((resolve) => {
    if (child.exitCode !== null) return resolve();
    child.once('exit', resolve);
    child.kill('SIGTERM');
  });

describe('server.ts process', { skip: adminUrl ? false : 'set DATABASE_URL to a role that may CREATE DATABASE' }, () => {
  let database;
  before(async () => {
    database = await ownedDatabase();
  });
  after(async () => {
    await database?.drop();
  });

  test('with no method configured it starts and reports none', async () => {
    const { child, ready } = await runProcess(baseEnv(database.url));
    try {
      assert.equal(ready.event, 'ready');
      assert.deepEqual(ready.methods, []);
      const base = `http://127.0.0.1:${ready.port}`;
      const health = await realFetch(`${base}/internal/health`, { headers: { 'x-runbook-internal-secret': INTERNAL_SECRET } });
      assert.deepEqual(await health.json(), { status: 'ok', methods: [] });
      const auth = await realFetch(`${base}/api/auth/ok`);
      assert.equal(auth.status, 503);
      assert.equal((await auth.json()).code, 'auth.not_configured');
    } finally {
      await stop(child);
    }
  });

  test('a misconfiguration stops startup with every problem listed', async () => {
    const { code, stdout, stderr } = await runProcess({ ...baseEnv(database.url), RUNBOOK_AUTH_METHODS: 'email,google,saml', RUNBOOK_GOOGLE_CLIENT_ID: 'only-the-id' });
    assert.equal(code, 78);
    assert.equal(stdout, '');
    for (const text of ['unknown method "saml"', 'RUNBOOK_SMTP_URL', 'RUNBOOK_GOOGLE_CLIENT_SECRET', 'RUNBOOK_AUTH_SECRET is required', 'RUNBOOK_ORIGIN is required']) assert.ok(stderr.includes(text), `${text} in:\n${stderr}`);
  });

  test('the capture transport refuses a non-loopback origin', async () => {
    const { code, stderr } = await runProcess({
      ...baseEnv(database.url),
      RUNBOOK_AUTH_EMAIL_TRANSPORT: 'capture',
      RUNBOOK_AUTH_CAPTURE_FILE: join(tmpdir(), 'never-written.jsonl'),
      RUNBOOK_AUTH_SECRET: AUTH_SECRET,
      RUNBOOK_ORIGIN: 'http://runbook.localhost.example',
      RUNBOOK_AUTH_CAN_REGISTER_URL: 'http://127.0.0.1:9/internal/auth/can-register',
    });
    assert.equal(code, 78);
    assert.match(stderr, /capture is test-only and refuses a non-loopback RUNBOOK_ORIGIN/);
  });

  // Compose passes every D48 variable, so an unset one arrives empty (AUTH5).
  test('empty D48 variables count as unset', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'runbook-auth-empty-'));
    const { child, ready, code, stderr } = await runProcess({
      ...baseEnv(database.url),
      RUNBOOK_AUTH_METHODS: '',
      RUNBOOK_AUTH_ALLOWED_DOMAINS: '',
      RUNBOOK_SMTP_URL: '',
      RUNBOOK_SMTP_FROM: '',
      RUNBOOK_GOOGLE_CLIENT_ID: '',
      RUNBOOK_GOOGLE_CLIENT_SECRET: '',
      RUNBOOK_AUTH_EMAIL_TRANSPORT: 'capture',
      RUNBOOK_AUTH_CAPTURE_FILE: join(dir, 'mail.jsonl'),
      RUNBOOK_AUTH_SECRET: AUTH_SECRET,
      RUNBOOK_ORIGIN: 'http://localhost:8080',
      RUNBOOK_AUTH_CAN_REGISTER_URL: 'http://127.0.0.1:9/internal/auth/can-register',
    });
    try {
      assert.equal(code, undefined, stderr);
      assert.deepEqual(ready.methods, ['email']);
    } finally {
      if (child) await stop(child);
      await rm(dir, { recursive: true, force: true });
    }
  });

  test('capture on a loopback origin: codes in JSONL, plain-http cookie name', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'runbook-auth-capture-'));
    const canRegister = await startCanRegister();
    const origin = 'http://runbook.localhost.:8080';
    canRegister.state.allowed.add('cap@example.test');
    const { child, ready } = await runProcess({
      ...baseEnv(database.url),
      RUNBOOK_AUTH_EMAIL_TRANSPORT: 'capture',
      RUNBOOK_AUTH_CAPTURE_FILE: join(dir, 'mail.jsonl'),
      RUNBOOK_AUTH_SECRET: AUTH_SECRET,
      RUNBOOK_ORIGIN: origin,
      RUNBOOK_AUTH_CAN_REGISTER_URL: canRegister.url,
    });
    try {
      assert.deepEqual(ready.methods, ['email']);
      const h = { ipHeader: 'x-runbook-client-ip', request: (path, init) => realFetch(`http://127.0.0.1:${ready.port}${path}`, { redirect: 'manual', ...init }) };
      assert.equal((await post(h, '/email-otp/send-verification-otp', { email: 'cap@example.test', type: 'sign-in' }, { origin, ip: '203.0.113.1' })).status, 200);
      const lines = (await readFile(join(dir, 'mail.jsonl'), 'utf8')).trim().split('\n').map((l) => JSON.parse(l));
      assert.equal(lines.length, 1);
      assert.deepEqual(Object.keys(lines[0]).sort(), ['at', 'code', 'subject', 'to', 'type']);
      assert.equal(lines[0].to, 'cap@example.test');
      const signIn = await post(h, '/sign-in/email-otp', { email: 'cap@example.test', otp: lines[0].code }, { origin, ip: '203.0.113.1' });
      assert.equal(signIn.status, 200, await signIn.clone().text());
      const set = signIn.headers.getSetCookie().find((c) => c.startsWith('runbook.session_token='));
      assert.ok(set, signIn.headers.getSetCookie().join(' | '));
      const attrs = set.split(';').map((a) => a.trim().toLowerCase());
      assert.ok(attrs.includes('httponly') && attrs.includes('samesite=lax') && !attrs.includes('secure'), set);
      const session = await realFetch(`http://127.0.0.1:${ready.port}/internal/session`, { method: 'POST', headers: { 'x-runbook-internal-secret': INTERNAL_SECRET, cookie: cookieHeader(signIn) } });
      assert.equal(session.status, 200);
    } finally {
      await stop(child);
      await canRegister.close();
      await rm(dir, { recursive: true, force: true });
    }
  });
});
