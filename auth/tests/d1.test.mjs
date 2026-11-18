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
import { behaviourSuite } from './support/suite.mjs';
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
  oauthSuite(h);

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
