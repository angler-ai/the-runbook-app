// CF7: a `PILOT_PSQL` stand-in for the Cloudflare edition's local stack.
// examples/pilot/seed-first-token.sh (and the web e2e harness) pipe
// examples/pilot/seed-first-token.sql to `$PILOT_PSQL -X -q -t -A -v
// ON_ERROR_STOP=1 -v token_id=… -v org_id=… -v user_id=… -v project_id=…
// -v hash_hex=…`. That statement is Postgres; this runs its equivalent on the
// stack's local D1 (`wrangler d1 execute --local`): the same guard (an
// active user of the organization that owns the project), the same
// credential payload (scopes, project, 90-day expiry and management name
// read from the SQL it was given), and the token ID printed when inserted.
//
//   PILOT_PSQL="node cloudflare/scripts/d1-psql.mjs <dev-stack state dir>"
//
// Only that one statement is understood; anything else is refused. Values
// are checked against their ID shapes before they reach SQL. LOCAL ONLY.
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ULID = '[0-7][0-9A-HJKMNP-TV-Z]{25}';
const SHAPES = {
  token_id: new RegExp(`^tok_${ULID}$`),
  org_id: new RegExp(`^org_${ULID}$`),
  user_id: new RegExp(`^usr_${ULID}$`),
  project_id: new RegExp(`^prj_${ULID}$`),
  hash_hex: /^[0-9a-f]{64}$/,
};

function fail(message) {
  process.stderr.write(`d1-psql: ${message}\n`);
  process.exit(2);
}

export function parseArgs(argv) {
  const vars = {};
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '-v') {
      const [key, ...rest] = argv[i + 1].split('=');
      vars[key] = rest.join('=');
      i += 1;
    }
  }
  return vars;
}

/** The D1 statement for seed-first-token.sql with `vars`. */
export function translate(sql, vars, nowMs = Date.now()) {
  if (!/INSERT INTO runbook\.credential/.test(sql) || !/'kind', 'personal'/.test(sql)) {
    throw new Error('only examples/pilot/seed-first-token.sql is understood');
  }
  for (const [key, shape] of Object.entries(SHAPES)) {
    if (!shape.test(vars[key] ?? '')) throw new Error(`-v ${key} is missing or malformed`);
  }
  const scopes = /'scopes', json_build_array\(([^)]*)\)/.exec(sql)?.[1];
  if (!scopes) throw new Error('no scopes in the statement');
  const scopeList = [...scopes.matchAll(/'([a-z_:]+)'/g)].map((m) => m[1]);
  const days = Number(/(\d+)::bigint \* 86400000/.exec(sql)?.[1] ?? NaN);
  const name = /'name', '([A-Za-z0-9 ()_-]+)'/.exec(sql)?.[1];
  if (!scopeList.length || !Number.isInteger(days) || !name) throw new Error('unexpected statement shape');
  const hash = [...Buffer.from(vars.hash_hex, 'hex')];
  const payload = {
    id: vars.token_id,
    kind: 'personal',
    hash,
    principal: { User: { user_id: vars.user_id, actor: 'human' } },
    org_id: vars.org_id,
    scopes: scopeList,
    projects: [vars.project_id],
    expires_at: nowMs + days * 86_400_000,
    revoked_at: null,
    last_used_at: null,
    management: { name, created_at: nowMs, created_by: null },
  };
  const json = JSON.stringify(payload).replaceAll("'", "''");
  return [
    'INSERT INTO credential (id, org_id, secret_hash, payload, revoked_at, last_used_at)',
    `SELECT '${vars.token_id}', u.org_id, X'${vars.hash_hex}', '${json}', NULL, NULL`,
    'FROM org_user u JOIN project p ON p.org_id = u.org_id',
    `WHERE u.org_id = '${vars.org_id}' AND u.user_id = '${vars.user_id}' AND u.active = 1`,
    `AND p.project_id = '${vars.project_id}'`,
    'RETURNING id;',
  ].join(' ');
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const state = process.argv[2];
  if (!state) fail('usage: d1-psql.mjs <dev-stack state dir> [psql arguments]');
  let statement;
  try {
    statement = translate(readFileSync(0, 'utf8'), parseArgs(process.argv.slice(3)));
  } catch (error) {
    fail(error.message);
  }
  const out = spawnSync(
    process.execPath,
    [
      join(root, 'node_modules', 'wrangler', 'bin', 'wrangler.js'),
      'd1',
      'execute',
      'DB',
      '--local',
      '--persist-to',
      join(state, 'data'),
      '-c',
      join(state, 'api', 'wrangler.json'),
      '--json',
      '--command',
      statement,
    ],
    {
      cwd: state,
      encoding: 'utf8',
      env: { ...process.env, WRANGLER_SEND_METRICS: 'false', CI: '1', CLOUDFLARE_API_TOKEN: '', CLOUDFLARE_ACCOUNT_ID: '' },
    },
  );
  if (out.status !== 0) fail(`wrangler d1 execute failed:\n${out.stderr}`);
  let rows = [];
  try {
    rows = JSON.parse(out.stdout).flatMap((r) => r.results ?? []);
  } catch {
    fail('unreadable wrangler output');
  }
  for (const row of rows) process.stdout.write(`${row.id}\n`);
}
