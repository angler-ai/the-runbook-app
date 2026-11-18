// Cloudflare Email Service as the auth Worker's transport
// (RUNBOOK_AUTH_EMAIL_TRANSPORT=cloudflare, D48): the code goes out through the
// `send_email` binding named EMAIL with Shelf's message shape (a structured
// `from`), a failed send voids the code, and no code reaches the logs.
// Miniflare with a WorkerEntrypoint standing in for the binding.
import { after, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response as MfResponse, convertV4MiniflareOptions } from 'miniflare';
import { ALLOWED_DOMAINS, AUTH_SECRET, INTERNAL_SECRET, ORIGIN, outboundPolicy, googleMock, startCanRegister } from './support/stubs.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const realFetch = globalThis.fetch;

// Records the whole message; a recipient starting `fail@` is refused, as a
// provider refusal would be.
const EMAIL_SERVICE = `
import { WorkerEntrypoint } from 'cloudflare:workers';
const sent = [];
export class Email extends WorkerEntrypoint {
  async send(message) {
    if (String(message.to).startsWith('fail@')) throw new Error('provider refused fail@example.test 123456');
    sent.push(message);
    return { messageId: 'test' };
  }
}
export default { fetch() { return Response.json(sent); } };`;

async function migrate(db) {
  const dir = join(root, 'drizzle/sqlite');
  for (const file of (await readdir(dir)).filter((f) => f.endsWith('.sql')).sort()) {
    const statements = (await readFile(join(dir, file), 'utf8')).split('--> statement-breakpoint').map((s) => s.trim()).filter(Boolean);
    await db.batch(statements.map((s) => db.prepare(s)));
  }
}

let mf;
let db;
let canRegister;
const logs = [];
const collect = (stream) => stream.on('data', (chunk) => logs.push(chunk.toString()));

before(async () => {
  canRegister = await startCanRegister();
  const outbound = outboundPolicy({ google: googleMock(), canRegister, realFetch });
  const script = await readFile(join(root, 'dist/worker.mjs'), 'utf8');
  mf = new Miniflare(
    convertV4MiniflareOptions({
      handleRuntimeStdio(stdout, stderr) {
        collect(stdout);
        collect(stderr);
      },
      workers: [
        {
          name: 'auth',
          modules: true,
          script,
          compatibilityDate: '2026-09-01',
          compatibilityFlags: ['nodejs_compat'],
          d1Databases: { DB: 'auth-cloudflare-email' },
          stripCfConnectingIp: false,
          bindings: {
            RUNBOOK_AUTH_INTERNAL_SECRET: INTERNAL_SECRET,
            RUNBOOK_AUTH_SECRET: AUTH_SECRET,
            RUNBOOK_ORIGIN: ORIGIN,
            RUNBOOK_AUTH_CAN_REGISTER_URL: canRegister.url,
            RUNBOOK_AUTH_ALLOWED_DOMAINS: ALLOWED_DOMAINS,
            RUNBOOK_AUTH_METHODS: 'email',
            RUNBOOK_AUTH_EMAIL_TRANSPORT: 'cloudflare',
            RUNBOOK_EMAIL_FROM: 'The Runbook (test) <runbook-test@example.test>',
          },
          serviceBindings: { EMAIL: { name: 'email', entrypoint: 'Email' } },
          outboundService: async (request) => {
            const r = await outbound.route(new URL(request.url), request.method, Object.fromEntries(request.headers), await request.text());
            return new MfResponse(r.body, { status: r.status, headers: r.headers });
          },
        },
        { name: 'email', modules: true, script: EMAIL_SERVICE, compatibilityDate: '2026-09-01' },
      ],
    }),
  );
  db = await mf.getD1Database('DB', 'auth');
  await migrate(db);
});

after(async () => {
  await mf?.dispose();
  await canRegister?.close();
});

const request = (email, ip) =>
  mf.dispatchFetch(`${ORIGIN}/api/auth/email-otp/send-verification-otp`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', origin: ORIGIN, 'cf-connecting-ip': ip },
    body: JSON.stringify({ email, type: 'sign-in' }),
  });
const sent = async () => (await (await mf.getWorker('email')).fetch('http://email/')).json();
const pending = async (email) =>
  (await db.prepare('SELECT COUNT(*) AS n FROM auth_verification WHERE identifier = ?').bind(`sign-in-otp-${email}`).first()).n;

test('a code goes out through EMAIL with a structured from, the recipient and the subject', async () => {
  const response = await request('someone@example.test', '198.51.100.1');
  assert.equal(response.status, 200, await response.clone().text());
  const [message] = await sent();
  assert.deepEqual(message.from, { name: 'The Runbook (test)', email: 'runbook-test@example.test' });
  assert.equal(message.to, 'someone@example.test');
  assert.match(message.subject, /^\d{6} is your The Runbook sign-in code$/);
  assert.match(message.text, /\d{6}/);
  assert.match(message.html, /\d{6}/);
  assert.equal(await pending('someone@example.test'), 1, 'the code is stored');
});

test('a failed send deletes the code, and no code reaches the logs', async () => {
  const response = await request('fail@example.test', '198.51.100.2');
  assert.ok(response.status >= 400, String(response.status));
  assert.equal(await pending('fail@example.test'), 0, 'an undelivered code is not usable');
  const [message] = await sent();
  const code = /^(\d{6}) /.exec(message.subject)[1];
  await new Promise((r) => setTimeout(r, 200));
  const text = logs.join('');
  assert.ok(!text.includes(code), 'a sent code is never logged');
  assert.ok(!text.includes('123456') && !text.includes('provider refused'), 'the provider error is never logged');
});
