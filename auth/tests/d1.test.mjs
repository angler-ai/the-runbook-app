// worker.ts on Miniflare with D1 (Shelf's harness): the esbuild bundle from
// `npm run build`, migrations applied by reading drizzle/sqlite, a WorkerEntrypoint
// standing in for the EMAIL binding, and every outbound fetch routed through
// `outboundService` (Google answered by a local key, can-register by a stub).
import { after, before, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response as MfResponse, convertV4MiniflareOptions } from 'miniflare';
import { statements as operatorStatements } from '../../cloudflare/scripts/mfa-reset.mjs';
import { behaviourSuite, post, signInByEmail } from './support/suite.mjs';
import { mfaSuite, totp } from './support/mfa.mjs';
import { oauthSuite } from './support/oauth.mjs';
import { ALLOWED_DOMAINS, AUTH_SECRET, GOOGLE_CLIENT_ID, GOOGLE_CLIENT_SECRET, INTERNAL_SECRET, ORIGIN, googleMock, outboundPolicy, startCanRegister } from './support/stubs.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const realFetch = globalThis.fetch;

const MAIL_WORKER = `
import { WorkerEntrypoint } from 'cloudflare:workers';
const messages = [];
export class Mail extends WorkerEntrypoint {
  async send(message) {
    if (message.to.startsWith('fail@')) throw new Error('delivery failed');
    messages.push({ to: message.to, code: /^(\\d{6}) /.exec(message.subject)?.[1] ?? null });
    return { messageId: 'test' };
  }
}
export default { fetch() { return Response.json(messages); } };`;

/** Applies drizzle-kit's SQLite migrations in order, reading the directory. */
async function migrate(db) {
  const dir = join(root, 'drizzle/sqlite');
  for (const file of (await readdir(dir)).filter((f) => f.endsWith('.sql')).sort()) {
    const statements = (await readFile(join(dir, file), 'utf8')).split('--> statement-breakpoint').map((s) => s.trim()).filter(Boolean);
    await db.batch(statements.map((s) => db.prepare(s)));
  }
}

describe('worker.ts on Miniflare/D1', () => {
  let mf;
  let db;
  let mail = [];
  const google = googleMock();
  let canRegister;
  let outbound;
  const h = {
    ipHeader: 'cf-connecting-ip',
    google,
    get canRegister() {
      return canRegister;
    },
    get outbound() {
      return outbound;
    },
    request: (path, init = {}) => mf.dispatchFetch(`${ORIGIN}${path}`, { redirect: 'manual', ...init }),
    mail: () => mail,
    async count(table, email) {
      const sql = {
        user: 'SELECT COUNT(*) AS n FROM auth_user WHERE email = ?',
        session: 'SELECT COUNT(*) AS n FROM auth_session s JOIN auth_user u ON u.id = s.user_id WHERE u.email = ?',
        verification: 'SELECT COUNT(*) AS n FROM auth_verification WHERE identifier = ?',
      }[table];
      return (await db.prepare(sql).bind(table === 'verification' ? `sign-in-otp-${email}` : email).first()).n;
    },
    async setSessionExpiry(id, ms) {
      await db.prepare('UPDATE auth_session SET expires_at = ? WHERE id = ?').bind(ms, id).run();
    },
    async mfaAudit(id) {
      return (await db.prepare('SELECT action, notification, notified_at FROM auth_mfa_audit WHERE user_id = ?').bind(id).all()).results;
    },
    async rejectMfaAudit() {
      await db.prepare(`CREATE TRIGGER fail_mfa_audit BEFORE INSERT ON auth_mfa_audit
        BEGIN SELECT RAISE(ABORT, 'test audit write failure'); END`).run();
    },
    async restoreMfaAudit() {
      await db.prepare('DROP TRIGGER fail_mfa_audit').run();
    },
    async setTotpSetupExpiry(id, ms) {
      await db.prepare('UPDATE auth_verification SET expires_at = ? WHERE identifier = ?').bind(ms, `totp-setup:${id}`).run();
    },
    async setSessionCreatedAt(id, ms) {
      await db.prepare('UPDATE auth_session SET created_at = ? WHERE id = ?').bind(ms, id).run();
    },
    async resetLimits() {
      await db.prepare("DELETE FROM auth_send_limit WHERE key NOT LIKE 'known:%'").run();
    },
    async verificationValue(email) {
      return (await db.prepare('SELECT value FROM auth_verification WHERE identifier = ?').bind(`sign-in-otp-${email}`).first())?.value ?? null;
    },
    async expireAccessTokens() {
      await db.prepare('UPDATE auth_oauth_access_token SET expires_at = ?').bind(Date.now() - 1000).run();
    },
    async backdateClient(clientId, ms) {
      await db.prepare('UPDATE auth_oauth_client SET created_at = ? WHERE client_id = ?').bind(ms, clientId).run();
    },
    async backdateConsent(clientId, ms) {
      await db.prepare('UPDATE auth_oauth_consent SET created_at = ?, updated_at = ? WHERE client_id = ?').bind(ms, ms, clientId).run();
    },
    async breakTokens() {
      await db.prepare('ALTER TABLE auth_oauth_access_token RENAME TO auth_oauth_access_token_hidden').run();
    },
    async fixTokens() {
      await db.prepare('ALTER TABLE auth_oauth_access_token_hidden RENAME TO auth_oauth_access_token').run();
    },
    async deleteClient(clientId) {
      await db.prepare('DELETE FROM auth_oauth_client WHERE client_id = ?').bind(clientId).run();
    },
    async deleteUser(email) {
      await db.prepare('DELETE FROM auth_user WHERE email = ?').bind(email).run();
    },
    async clientExists(clientId) {
      return (await db.prepare('SELECT COUNT(*) AS n FROM auth_oauth_client WHERE client_id = ?').bind(clientId).first()).n === 1;
    },
  };

  before(async () => {
    canRegister = await startCanRegister();
    outbound = outboundPolicy({ google, canRegister, realFetch });
    const script = await readFile(join(root, 'dist/worker.mjs'), 'utf8');
    const worker = (name, bindings, extra = {}) => ({
      name,
      modules: true,
      script,
      compatibilityDate: '2026-09-01',
      compatibilityFlags: ['nodejs_compat'],
      d1Databases: { DB: `auth-${name}` },
      stripCfConnectingIp: false,
      bindings: { RUNBOOK_AUTH_INTERNAL_SECRET: INTERNAL_SECRET, ...bindings },
      outboundService: async (request) => {
        const r = await outbound.route(new URL(request.url), request.method, Object.fromEntries(request.headers), await request.text());
        return new MfResponse(r.body, { status: r.status, headers: r.headers });
      },
      ...extra,
    });
    mf = new Miniflare(
      convertV4MiniflareOptions({
        workers: [
          worker(
            'auth',
            {
              RUNBOOK_AUTH_SECRET: AUTH_SECRET,
              RUNBOOK_ORIGIN: ORIGIN,
              RUNBOOK_AUTH_CAN_REGISTER_URL: canRegister.url,
              RUNBOOK_AUTH_ALLOWED_DOMAINS: ALLOWED_DOMAINS,
              RUNBOOK_SMTP_FROM: 'The Runbook <login@example.test>',
              RUNBOOK_GOOGLE_CLIENT_ID: GOOGLE_CLIENT_ID,
              RUNBOOK_GOOGLE_CLIENT_SECRET: GOOGLE_CLIENT_SECRET,
            },
            { serviceBindings: { EMAIL: { name: 'mail', entrypoint: 'Mail' } } },
          ),
          worker('nomethods', {}),
          worker('misconfigured', { RUNBOOK_AUTH_METHODS: 'email', RUNBOOK_AUTH_SECRET: AUTH_SECRET, RUNBOOK_ORIGIN: ORIGIN }),
          { name: 'mail', modules: true, script: MAIL_WORKER, compatibilityDate: '2026-09-01' },
        ],
      }),
    );
    db = await mf.getD1Database('DB', 'auth');
    await migrate(db);
    await migrate(await mf.getD1Database('DB', 'nomethods'));
    const mailWorker = await mf.getWorker('mail');
    h.mail = () => mail;
    // Refresh the delivered-mail list before each read.
    const refresh = async () => {
      mail = await (await mailWorker.fetch('http://mail/')).json();
    };
    // Buffer each response into a plain Node Response before the test reads it:
    // Miniflare's undici bodies occasionally report "Body is unusable" under load.
    const original = h.request;
    h.request = async (path, init) => {
      const response = await original(path, init);
      const body = [204, 304].includes(response.status) ? null : await response.arrayBuffer();
      await refresh();
      return new Response(body, { status: response.status, headers: response.headers });
    };
  });

  after(async () => {
    await mf?.dispose();
    await canRegister?.close();
  });

  behaviourSuite(h);
  mfaSuite(h);
  oauthSuite(h);

  test('MFA migration accepts legacy writes after reset and fences explicit stale generations', async () => {
    const account = 'legacy-d1-account';
    await db.prepare("INSERT INTO auth_user (id, name, email) VALUES (?, 'Legacy', 'legacy-d1@example.test')").bind(account).run();
    await db.prepare('UPDATE auth_user SET mfa_generation = 1 WHERE id = ?').bind(account).run();
    const session = 'legacy-d1-session';
    const now = Date.now();
    await db.prepare('INSERT INTO auth_session (id, token, user_id, expires_at, updated_at) VALUES (?, ?, ?, ?, ?)')
      .bind(session, session, account, now + 60_000, now).run();
    await db.prepare(`INSERT INTO auth_passkey (id, public_key, user_id, credential_id, counter, device_type, backed_up)
      VALUES (?, 'pk', ?, ?, 0, 'singleDevice', 0)`).bind('legacy-d1-passkey', account, 'legacy-d1-credential').run();
    assert.equal(await db.prepare('SELECT mfa_generation FROM auth_session WHERE id = ?').bind(session).first('mfa_generation'), 1);
    assert.equal(await db.prepare('SELECT mfa_generation FROM auth_passkey WHERE user_id = ?').bind(account).first('mfa_generation'), 1);
    await assert.rejects(db.prepare(`INSERT INTO auth_session (id, token, user_id, expires_at, updated_at, mfa_generation)
      VALUES ('stale-d1-session', 'stale-d1-session', ?, ?, ?, 0)`).bind(account, now + 60_000, now).run(), /MFA generation changed/);

    await db.batch([
      db.prepare("INSERT INTO auth_oauth_client (id, client_id, redirect_uris) VALUES ('legacy-client', 'legacy-client', '[]')"),
      db.prepare(`INSERT INTO auth_oauth_refresh_token (id, token, client_id, session_id, user_id, scopes)
        VALUES ('legacy-refresh', 'legacy-refresh', 'legacy-client', ?, ?, '[]')`).bind(session, account),
      db.prepare(`INSERT INTO auth_oauth_access_token (id, token, client_id, refresh_id, session_id, user_id, scopes)
        VALUES ('legacy-access', 'legacy-access', 'legacy-client', 'legacy-refresh', ?, ?, '[]')`).bind(session, account),
    ]);
    assert.deepEqual(await db.prepare("SELECT mfa_verified_at, mfa_method FROM auth_oauth_access_token WHERE id = 'legacy-access'").first(),
      { mfa_verified_at: null, mfa_method: null });
  });

  test('MFA: the D1 operator resets factors and queues mail without an HTTP route', async () => {
    const email = 'operator-recovery@example.test';
    h.canRegister.state.allowed.add(email);
    const { cookie } = await signInByEmail(h, email);
    const readSession = (cookie) => h.request('/internal/session', {
      method: 'POST', headers: { cookie, 'x-runbook-internal-secret': INTERNAL_SECRET },
    });
    const { auth_user_id: id } = await (await readSession(cookie)).json();
    const setup = await post(h, '/two-factor/enable', {}, { cookie });
    assert.equal(setup.status, 200);
    const { totpURI } = await setup.json();
    assert.equal((await post(h, '/two-factor/verify-totp', { code: totp(totpURI) }, { cookie })).status, 200);
    await db.batch(operatorStatements(email, "D1 Test Operator").reset(id, 'operator-reset-test').map((sql) => db.prepare(sql)));
    assert.equal((await readSession(cookie)).status, 401);
    const reset = () => db.prepare("SELECT actor, notification, notified_at FROM auth_mfa_audit WHERE user_id = ? AND action = 'mfa.reset'").bind(id).first();
    assert.deepEqual(await reset(), { actor: 'D1 Test Operator', notification: 'reset', notified_at: null });
    const worker = await mf.getWorker('auth');
    const cron = await worker.scheduled({ cron: '* * * * *', scheduledTime: new Date() });
    assert.equal(cron.outcome, 'ok');
    assert.ok((await reset()).notified_at);
    await h.resetLimits();
    const next = await signInByEmail(h, email);
    assert.equal((await (await readSession(next.cookie)).json()).mfa_enrolled, false);
  });

  test('with no method configured the Worker runs and reports none', async () => {
    const worker = await mf.getWorker('nomethods');
    const health = await worker.fetch(`${ORIGIN}/internal/health`, { headers: { 'x-runbook-internal-secret': INTERNAL_SECRET } });
    assert.deepEqual(await health.json(), { status: 'ok', methods: [] });
    const auth = await worker.fetch(`${ORIGIN}/api/auth/ok`);
    assert.equal(auth.status, 503);
    assert.equal((await auth.json()).code, 'auth.not_configured');
    const session = await worker.fetch(`${ORIGIN}/internal/session`, { method: 'POST', headers: { 'x-runbook-internal-secret': INTERNAL_SECRET, cookie: '__Secure-runbook.session_token=x.y' } });
    assert.equal(session.status, 401);
  });

  test('a misconfigured Worker refuses every request with 503', async () => {
    const worker = await mf.getWorker('misconfigured');
    for (const path of ['/api/auth/ok', '/internal/health']) {
      const r = await worker.fetch(`${ORIGIN}${path}`, { headers: { 'x-runbook-internal-secret': INTERNAL_SECRET } });
      assert.equal(r.status, 503);
      assert.equal((await r.json()).code, 'auth.misconfigured');
    }
  });
});
