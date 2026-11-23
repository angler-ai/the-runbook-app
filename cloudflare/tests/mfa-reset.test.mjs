// The Workers operator MFA reset (scripts/mfa-reset.mjs): its wrangler
// commands run against a real D1 (Miniflare) holding the sign-in Worker's
// migrations, so the reset runs as an audited D1 batch and the quoting is
// checked by SQLite itself. No Cloudflare account is touched. LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { Miniflare, convertV4MiniflareOptions } from 'miniflare';
import { root } from '../scripts/dev-stack.mjs';
import { EXIT, literal, main, parseArgs } from '../scripts/mfa-reset.mjs';

const CONFIG = join(root, 'wrangler.auth.test-reset.jsonc');
let mf;
let db;

before(async () => {
  mf = new Miniflare(
    convertV4MiniflareOptions({
      workers: [
        {
          name: 'reset',
          modules: true,
          script: 'export default { fetch() { return new Response(null, { status: 204 }); } }',
          compatibilityDate: '2026-09-01',
          d1Databases: ['DB'],
        },
      ],
    }),
  );
  await mf.ready;
  db = await mf.getD1Database('DB', 'reset');
  const dir = join(root, '..', 'auth', 'drizzle', 'sqlite');
  for (const file of (await readdir(dir)).filter((f) => f.endsWith('.sql')).sort()) {
    const sql = await readFile(join(dir, file), 'utf8');
    for (const statement of sql.split('--> statement-breakpoint').map((s) => s.trim()).filter(Boolean)) {
      await db.prepare(statement).run();
    }
  }
});

after(async () => {
  await mf?.dispose();
});

async function seed(id, email) {
  const now = Date.now();
  await db.batch([
    db.prepare('INSERT INTO auth_user (id, name, email, email_verified, two_factor_enabled) VALUES (?, ?, ?, 1, 1)').bind(id, 'Owner', email),
    db.prepare("INSERT INTO auth_passkey (id, public_key, user_id, credential_id, counter, device_type, backed_up) VALUES (?, 'pk', ?, ?, 0, 'singleDevice', 0)").bind(`${id}-pk`, id, `${id}-cred`),
    db.prepare("INSERT INTO auth_two_factor (id, secret, backup_codes, user_id, verified) VALUES (?, 's', '[]', ?, 1)").bind(`${id}-tf`, id),
    db.prepare('INSERT INTO auth_session (id, expires_at, token, updated_at, user_id) VALUES (?, ?, ?, ?, ?)').bind(`${id}-s1`, now + 86_400_000, `${id}-t1`, now, id),
    db.prepare('INSERT INTO auth_session (id, expires_at, token, updated_at, user_id) VALUES (?, ?, ?, ?, ?)').bind(`${id}-s2`, now + 86_400_000, `${id}-t2`, now, id),
  ]);
}

const count = async (table, id) =>
  (await db.prepare(`SELECT count(*) AS n FROM ${table} WHERE user_id = ?`).bind(id).first()).n;

/** Answers `wrangler d1 execute --json --command` from the Miniflare D1, as wrangler formats it. */
function deps({ tty = false, confirm = true, loggedIn = true, config = true } = {}) {
  const calls = [];
  const lines = [];
  return {
    calls,
    lines,
    root,
    isTTY: tty,
    exists: (path) => config && path === CONFIG,
    readConfig: () => ({ name: 'runbook-auth', d1_databases: [{ binding: 'DB', database_name: 'runbook-production' }] }),
    readSelf: () => '// help\nimport x',
    confirm: async () => confirm,
    log: (line) => lines.push(line),
    wrangler: async (args) => {
      calls.push(args);
      if (args[0] === 'whoami') return { status: loggedIn ? 0 : 1, stdout: '', stderr: '' };
      assert.deepEqual(args.slice(0, 3), ['d1', 'execute', 'DB']);
      assert.ok(args.includes('--json'));
      const command = args[args.indexOf('--command') + 1];
      try {
        const fileIndex = args.indexOf('--file');
        const results = fileIndex === -1
          ? [await db.prepare(command).all()]
          : await db.batch((await readFile(args[fileIndex + 1], 'utf8'))
            .split(';\n').filter(Boolean).map((sql) => db.prepare(sql)));
        // Release gate 2026-11-21: a remote --file import prints its upload
        // progress on stdout before the JSON, even with --json (wrangler 4).
        const preamble = fileIndex === -1 ? ''
          : '├ Checking if file needs uploading\n│\n├ 🌀 Uploading db.sql\n│ 🌀 Uploading complete.\n│\n';
        return { status: 0, stdout: preamble + JSON.stringify(results, null, 2), stderr: '' };
      } catch (error) {
        return { status: 1, stdout: '', stderr: String(error.message) };
      }
    },
  };
}

const args = (...rest) => [...rest, '--operator', "Alex O'Brien", '--auth-config', CONFIG];

test('resets an owner through an audited D1 batch and reads the audit back', async () => {
  // An apostrophe is legal in an address and must not break out of the literal.
  const email = "o'brien@example.test";
  await seed('owner-1', email);
  await seed('bystander', 'bystander@example.test');
  const d = deps();
  assert.equal(await main(args('O\'Brien@Example.test', '--yes'), d), EXIT.OK);
  assert.deepEqual(d.calls[0], ['whoami', '--json']);
  for (const call of d.calls.slice(1)) {
    assert.ok(call.includes('--remote'));
    assert.equal(call.at(-1), CONFIG);
  }
  assert.match(d.lines[0], /1 passkey\(s\), 1 authenticator app\(s\), 2 session\(s\), 0 OAuth token\(s\)/);
  assert.match(d.lines.join('\n'), /Reset o'brien@example\.test: audit mfa\.reset by Alex O'Brien/);
  assert.match(d.lines.join('\n'), /queued; the auth Worker's one-minute Cron Trigger/);
  for (const table of ['auth_passkey', 'auth_two_factor', 'auth_session']) {
    assert.equal(await count(table, 'owner-1'), 0, table);
    assert.ok((await count(table, 'bystander')) > 0, `${table} of another account is untouched`);
  }
  const audit = await db.prepare("SELECT actor, notification, notified_at FROM auth_mfa_audit WHERE user_id = 'owner-1' AND action = 'mfa.reset'").first();
  assert.deepEqual(audit, { actor: "Alex O'Brien", notification: 'reset', notified_at: null });
  const user = await db.prepare("SELECT mfa_generation, two_factor_enabled FROM auth_user WHERE id = 'owner-1'").first();
  assert.deepEqual(user, { mfa_generation: 1, two_factor_enabled: 0 });
});

test('refuses an account whose displayed address loses verification before confirmation', async () => {
  const id = 'confirmation-race';
  const email = 'confirmation-race@example.test';
  await seed(id, email);
  await db.prepare(`INSERT INTO auth_mfa_audit (id, user_id, action, actor, occurred_at)
    VALUES ('prior-reset', ?, 'mfa.reset', 'operator', ?)`).bind(id, Date.now()).run();
  const d = deps({ tty: true });
  d.confirm = async () => {
    await db.prepare('UPDATE auth_user SET email_verified = 0 WHERE id = ?').bind(id).run();
    return true;
  };
  assert.equal(await main(args(email), d), EXIT.FAILED);
  assert.equal(await count('auth_passkey', id), 1);
  const { n } = await db.prepare("SELECT count(*) AS n FROM auth_mfa_audit WHERE user_id = ? AND action = 'mfa.reset'").bind(id).first();
  assert.equal(n, 1);
});

test('an audit failure rolls back the entire reset', async () => {
  const id = 'audit-failure';
  const email = 'audit-failure@example.test';
  await seed(id, email);
  await db.prepare(`CREATE TRIGGER reject_operator_audit BEFORE INSERT ON auth_mfa_audit
    WHEN NEW.action = 'mfa.reset' AND NEW.user_id = '${id}'
    BEGIN SELECT RAISE(ABORT, 'test audit failure'); END`).run();
  try {
    const d = deps();
    assert.equal(await main(args(email, '--yes'), d), EXIT.FAILED);
    assert.match(d.lines.at(-1), /wrangler d1 execute failed \(reset\)/);
    assert.equal(await count('auth_session', id), 2);
    assert.equal(await count('auth_passkey', id), 1);
    assert.equal(await count('auth_two_factor', id), 1);
    const account = await db.prepare('SELECT mfa_generation, two_factor_enabled FROM auth_user WHERE id = ?').bind(id).first();
    assert.deepEqual(account, { mfa_generation: 0, two_factor_enabled: 1 });
  } finally {
    await db.prepare('DROP TRIGGER reject_operator_audit').run();
  }
});

test('asks before changing anything, and a no keeps everything', async () => {
  await seed('owner-2', 'second@example.test');
  const declined = deps({ tty: true, confirm: false });
  assert.equal(await main(args('second@example.test'), declined), EXIT.REFUSED);
  assert.equal(declined.calls.filter((c) => c.includes('--command')).length, 1, 'only the lookup ran');
  assert.equal(await count('auth_passkey', 'owner-2'), 1);
  const notTerminal = deps();
  assert.equal(await main(args('second@example.test'), notTerminal), EXIT.REFUSED);
  assert.match(notTerminal.lines[0], /pass --yes/);
  assert.equal(notTerminal.calls.length, 0);
  const local = deps({ tty: true, confirm: true });
  assert.equal(await main(args('second@example.test', '--local'), local), EXIT.OK);
  assert.ok(local.calls.every((c) => c.includes('--local') && !c.includes('--remote')));
  assert.ok(!local.calls.some((c) => c[0] === 'whoami'));
});

test('refuses an unknown account, a missing config, no login and a bad address', async () => {
  const unknown = deps();
  assert.equal(await main(args('nobody@example.test', '--yes'), unknown), EXIT.REFUSED);
  assert.match(unknown.lines.at(-1), /no auth account .* nobody@example\.test/);
  assert.equal(unknown.calls.filter((c) => c.includes('--command')).length, 1);
  const missing = deps({ config: false });
  assert.equal(await main(args('a@example.test', '--yes'), missing), EXIT.REFUSED);
  assert.match(missing.lines[0], /no sign-in Worker config/);
  const loggedOut = deps({ loggedIn: false });
  assert.equal(await main(args('a@example.test', '--yes'), loggedOut), EXIT.REFUSED);
  assert.match(loggedOut.lines[0], /wrangler is not logged in/);
  for (const bad of ['not-an-email', 'a@b.test\nDROP', 'a b@c.test']) {
    assert.equal(await main(args(bad, '--yes'), deps()), EXIT.USAGE, JSON.stringify(bad));
  }
  assert.equal(await main(['a@example.test', 'b@example.test'], deps()), EXIT.USAGE);
  assert.equal(await main(['a@example.test', '--yes', '--auth-config', CONFIG], deps()), EXIT.USAGE);
  assert.equal(await main(['--help'], deps()), EXIT.OK);
});

test('quotes literals for SQLite and normalizes the address', () => {
  assert.equal(literal("x'); DROP TABLE auth_user; --"), "'x''); DROP TABLE auth_user; --'");
  assert.equal(parseArgs(args(' Ada@Example.TEST ')).email, 'ada@example.test');
});
