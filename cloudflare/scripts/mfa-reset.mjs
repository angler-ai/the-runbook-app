#!/usr/bin/env node
// Operator MFA reset for the Cloudflare edition: the Workers equivalent of
// `runbook-server admin mfa reset <email>`.
//
//   npm run mfa:reset -- owner@example.test --operator "Alex Doe"            (from cloudflare/)
//   npm run mfa:reset -- owner@example.test --operator "Alex Doe" --yes --auth-config wrangler.auth.prod.jsonc
//
// For the last owner, or a member of several organizations (or with a sign-in
// shared across them), whom no organization owner may reset. With your
// wrangler login, against the sign-in Worker's D1 database (`DB` in its
// config), it:
//   1. looks the account up by its verified sign-in email and shows what it
//      holds (passkeys, authenticator, sessions, OAuth tokens);
//   2. after you confirm, runs the reset as one D1 SQL import. It invalidates
//      pending ceremonies, removes every factor, revokes sessions and OAuth
//      tokens, and queues the security notification in the operator audit row;
//   3. reads the audit row back. The auth Worker's one-minute Cron Trigger
//      (`* * * * *`) delivers the notification, as native auth does.
// The person then signs in with an email code and, where MFA is required,
// creates an allowed passkey.
//
// Options:
//   --operator NAME     the person performing the reset (required)
//   --auth-config FILE   the sign-in Worker's config (default wrangler.auth.prod.jsonc)
//   --local              the local D1 of `wrangler dev` instead of --remote
//   --yes                do not ask (required when not on a terminal)
//   --help
//
// Exit status: 0 reset, 1 refused or nothing changed, 2 usage, 4 failed.
// Prints no secrets and no factor material.
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { createInterface } from 'node:readline/promises';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');

export const EXIT = { OK: 0, REFUSED: 1, USAGE: 2, FAILED: 4 };

export class Refusal extends Error {
  constructor(message, code = EXIT.REFUSED) {
    super(message);
    this.code = code;
  }
}

// The sign-in service's own check (`auth/src/server.ts`, mfa-reset).
const EMAIL = /^[^\s@]+@[^\s@]+$/;

export function parseArgs(argv) {
  const opts = { authConfig: 'wrangler.auth.prod.jsonc', local: false, yes: false, help: false, email: null, operator: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--help' || a === '-h') opts.help = true;
    else if (a === '--yes' || a === '-y') opts.yes = true;
    else if (a === '--local') opts.local = true;
    else if (a === '--operator') {
      const name = argv[++i]?.trim();
      if (!name || name.startsWith('-') || name.length > 256 || /[\u0000-\u001f\u007f]/.test(name)) {
        throw new Refusal('--operator needs a valid name', EXIT.USAGE);
      }
      opts.operator = name;
    } else if (a === '--auth-config') {
      const value = argv[++i];
      if (!value || value.startsWith('--')) throw new Refusal('--auth-config needs a file', EXIT.USAGE);
      opts.authConfig = value;
    } else if (a.startsWith('-')) throw new Refusal(`unknown option ${a}`, EXIT.USAGE);
    else if (opts.email === null) opts.email = a;
    else throw new Refusal('one email address only', EXIT.USAGE);
  }
  if (opts.help) return opts;
  if (opts.email === null) throw new Refusal('usage: npm run mfa:reset -- <email> --operator NAME [--yes] [--auth-config FILE] [--local]', EXIT.USAGE);
  const email = opts.email.trim().toLowerCase();
  // No control characters reach the SQL text; quotes are doubled below.
  if (!EMAIL.test(email) || /[\u0000-\u001f\u007f]/.test(email) || email.length > 254) {
    throw new Refusal('a valid email address is required', EXIT.USAGE);
  }
  if (!opts.operator) throw new Refusal('--operator NAME is required', EXIT.USAGE);
  opts.email = email;
  return opts;
}

/** A SQLite string literal. `wrangler d1 execute --command` takes no parameters. */
export function literal(text) {
  return `'${text.replaceAll("'", "''")}'`;
}

export function statements(email, operator) {
  const e = literal(email);
  return {
    lookup: `SELECT u.id AS id,
  (SELECT count(*) FROM auth_passkey p WHERE p.user_id = u.id) AS passkeys,
  (SELECT count(*) FROM auth_two_factor t WHERE t.user_id = u.id AND t.verified) AS authenticators,
  (SELECT count(*) FROM auth_session s WHERE s.user_id = u.id) AS sessions,
  (SELECT count(*) FROM auth_oauth_access_token a WHERE a.user_id = u.id)
    + (SELECT count(*) FROM auth_oauth_refresh_token r WHERE r.user_id = u.id) AS oauth_tokens
FROM auth_user u WHERE u.email = ${e} AND u.email_verified = TRUE`,
    reset: (id, auditId) => {
      const account = `id = ${literal(id)} AND email = ${e} AND email_verified = TRUE`;
      const target = `(SELECT id FROM auth_user WHERE ${account})`;
      return [
        `UPDATE auth_user SET mfa_generation = mfa_generation + 1, two_factor_enabled = FALSE WHERE ${account}`,
        `DELETE FROM auth_verification WHERE identifier IN (
          SELECT 'totp-setup:' || id FROM auth_session WHERE user_id = ${target}
        )`,
        ...['oauth_access_token', 'oauth_refresh_token', 'session', 'passkey', 'two_factor'].map(
          (table) => `DELETE FROM auth_${table} WHERE user_id = ${target}`,
        ),
        `DELETE FROM auth_send_limit WHERE EXISTS (
          SELECT 1 FROM auth_user WHERE ${account}
          AND (key = 'mfa:' || id OR key = 'mfa-lock-audit:' || id OR instr(key, 'mfa-attempt:' || id || ':') = 1
            OR instr(key, 'mfa-account-attempt:' || id || ':') = 1
            OR instr(key, 'mfa-lock-audit:account:' || id || ':') = 1)
        )`,
        `INSERT INTO auth_mfa_audit (id, user_id, action, actor, occurred_at, notification)
          SELECT ${literal(auditId)}, id, 'mfa.reset', ${literal(operator)},
            CAST(unixepoch('subsecond') * 1000 AS integer), 'reset'
          FROM auth_user WHERE ${account}`,
      ];
    },
    audit: (id) => `SELECT action, actor, notification, notified_at, occurred_at
FROM auth_mfa_audit WHERE id = ${literal(id)}`,
  };
}

/** The rows of one `wrangler d1 execute --json` answer. */
function rows(out, what) {
  if (out.status !== 0) {
    throw new Refusal(`wrangler d1 execute failed (${what})`, EXIT.FAILED);
  }
  let parsed;
  try {
    // A remote --file import prints upload progress before the JSON, even
    // with --json: read from the first line that opens the JSON answer.
    const start = out.stdout.search(/^[[{]/m);
    parsed = JSON.parse(start === -1 ? out.stdout : out.stdout.slice(start));
  } catch {
    throw new Refusal(`wrangler answered ${what} with something other than JSON`, EXIT.FAILED);
  }
  const first = Array.isArray(parsed) ? parsed[0] : parsed;
  if (!first || first.success === false || !Array.isArray(first.results)) {
    throw new Refusal(`wrangler d1 execute failed (${what})`, EXIT.FAILED);
  }
  return first.results;
}

export async function main(argv, deps) {
  let opts;
  try {
    opts = parseArgs(argv);
    if (opts.help) {
      deps.log(deps.readSelf().split('\nimport ')[0].replace(/^#!.*\n/, '').replace(/^\/\/ ?/gm, ''));
      return EXIT.OK;
    }
    if (!opts.yes && !deps.isTTY) throw new Refusal('not a terminal: pass --yes to reset without asking');
    const path = isAbsolute(opts.authConfig) ? opts.authConfig : join(deps.root ?? root, opts.authConfig);
    if (!deps.exists(path)) {
      throw new Refusal(`no sign-in Worker config at ${path}: pass --auth-config (README §0)`);
    }
    const config = deps.readConfig(path);
    const database = config.d1_databases?.find((d) => d.binding === 'DB');
    if (!database) throw new Refusal(`${path} binds no D1 database as DB`);
    if (!opts.local && (await deps.wrangler(['whoami', '--json'], { capture: true })).status !== 0) {
      throw new Refusal('wrangler is not logged in: run `npx wrangler login` (or set CLOUDFLARE_API_TOKEN)');
    }
    const sql = statements(opts.email, opts.operator);
    const execute = async (input, what) =>
      rows(
        await deps.wrangler(
          ['d1', 'execute', 'DB', opts.local ? '--local' : '--remote', '--json', ...input, '-c', path],
          { capture: true, env: { CI: '1' } },
        ),
        what,
      );
    const where = `${database.database_name ?? 'DB'} (${opts.local ? 'local' : 'remote'})`;
    const [account] = await execute(['--command', sql.lookup], 'account lookup');
    if (!account) throw new Refusal(`no auth account in ${where} has the email ${opts.email}`);
    deps.log(`==> ${opts.email} in ${where}: ${account.passkeys} passkey(s), ${account.authenticators} authenticator app(s), ${account.sessions} session(s), ${account.oauth_tokens} OAuth token(s)`);
    deps.log('    The reset removes every factor and backup code, ends every session and OAuth grant token, and queues a security email.');
    if (!opts.yes && !(await deps.confirm(`Reset the second factors of ${opts.email}?`))) {
      throw new Refusal('nothing changed');
    }
    const auditId = randomUUID();
    const dir = await mkdtemp(join(tmpdir(), 'runbook-mfa-reset-'));
    try {
      const file = join(dir, 'reset.sql');
      await writeFile(file, sql.reset(account.id, auditId).join(';\n') + ';\n', { mode: 0o600 });
      await execute(['--file', file, '--yes'], 'reset');
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
    const [audit] = await execute(['--command', sql.audit(auditId)], 'audit check');
    if (!audit) throw new Refusal('the reset ran but no operator audit record was found; check auth_mfa_audit', EXIT.FAILED);
    deps.log(`==> Reset ${opts.email}: audit ${audit.action} by ${audit.actor} at ${new Date(Number(audit.occurred_at)).toISOString()}.`);
    deps.log(
      audit.notified_at
        ? '    The security email was sent.'
        : '    The security email is queued; the auth Worker\'s one-minute Cron Trigger sends it (configure `* * * * *` on the auth Worker if it has none).',
    );
    deps.log('    They sign in with an email code next and, where MFA is required, create an allowed passkey.');
    return EXIT.OK;
  } catch (error) {
    if (error instanceof Refusal) {
      deps.log(`error: ${error.message}`);
      return error.code;
    }
    throw error;
  }
}

export async function realDeps() {
  const wrangler = await import('wrangler');
  const wranglerBin = join(root, 'node_modules', 'wrangler', 'bin', 'wrangler.js');
  const { readFileSync } = await import('node:fs');
  return {
    root,
    exists: existsSync,
    readConfig: (path) => structuredClone(wrangler.experimental_readRawConfig({ config: path }).rawConfig),
    readSelf: () => readFileSync(fileURLToPath(import.meta.url), 'utf8'),
    wrangler: async (args, { capture = false, env = {} } = {}) => {
      const out = spawnSync(process.execPath, [wranglerBin, ...args], {
        cwd: root,
        env: { ...process.env, WRANGLER_SEND_METRICS: 'false', ...env },
        encoding: 'utf8',
        stdio: capture ? ['ignore', 'pipe', 'pipe'] : 'inherit',
      });
      return { status: out.status ?? 1, stdout: out.stdout ?? '', stderr: out.stderr ?? '' };
    },
    confirm: async (question) => {
      const rl = createInterface({ input: process.stdin, output: process.stdout });
      const answer = await rl.question(`${question} [y/N] `);
      rl.close();
      return /^y(es)?$/i.test(answer.trim());
    },
    log: (m) => console.log(m),
    isTTY: Boolean(process.stdin.isTTY),
  };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = await main(process.argv.slice(2), await realDeps());
}
