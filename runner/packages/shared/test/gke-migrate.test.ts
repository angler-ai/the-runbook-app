import { execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { readFileSync } from 'node:fs';
import pg from 'pg';
import { expect, it } from 'vitest';
import { openPool, sqlIdentifier } from '../src/db.ts';
import { TEST_DATABASE_URL } from '../test-utils/pg.ts';

it.skipIf(!TEST_DATABASE_URL)('prepares a PostgreSQL 16 owner and migrates a prefixed GKE database', async () => {
  const prefix = 'gke_' + randomBytes(6).toString('hex');
  const [operator, bootstrap, api, vault] = ['operator', 'bootstrap', 'ap.i', 'vault'].map((name) => `${prefix}-${name}`) as
    [string, string, string, string];
  const core = `${prefix}-core`;
  const runner = `${prefix}-runner`;
  const q = sqlIdentifier;
  const root = new pg.Client({ connectionString: TEST_DATABASE_URL });
  const previousPrefix = process.env.RUNNER_DB_ROLE_PREFIX;
  const password = 'gke-migration-test';
  function url(database: string, user: string) {
    const target = new URL(TEST_DATABASE_URL!);
    target.pathname = `/${database}`;
    target.username = user;
    target.password = password;
    return target.toString();
  }
  const pools: pg.Pool[] = [];
  await root.connect();
  try {
    await root.query(`CREATE ROLE ${q(operator)} LOGIN CREATEROLE CREATEDB PASSWORD '${password}'`);
    for (const role of [bootstrap, api, vault]) {
      await root.query(`CREATE ROLE ${q(role)} LOGIN PASSWORD '${password}'`);
      await root.query(`GRANT ${q(role)} TO ${q(operator)} WITH ADMIN TRUE, INHERIT FALSE, SET FALSE`);
    }
    for (const database of [core, runner]) await root.query(`CREATE DATABASE ${q(database)} OWNER ${q(operator)}`);
    const admin = new pg.Client({ connectionString: url(core, operator) });
    await admin.connect();
    try {
      const guide = readFileSync(new URL('../../../../site/src/content/docs/runner/gke.mdx', import.meta.url), 'utf8');
      const sql = guide.split('## Prepare Cloud SQL once')[1]!.split('```sql\n')[1]!.split('```')[0]!;
      await admin.query(sql.replace(/\b(?:runbook_runner|runbook)\b|"(?:CORE_DATABASE_LOGIN|RUNNER_BOOTSTRAP_LOGIN)"/g,
        (name) => ({
          runbook: q(core), runbook_runner: q(runner), '"CORE_DATABASE_LOGIN"': q(operator),
          '"RUNNER_BOOTSTRAP_LOGIN"': q(bootstrap),
        })[name]!));
      for (const [login, limit] of [[api, 2], [vault, 1], [bootstrap, 2]] as const) {
        await admin.query(`ALTER ROLE ${q(login)} CONNECTION LIMIT ${limit};
          ALTER ROLE ${q(login)} SET idle_in_transaction_session_timeout = '15s'`);
      }
      const retained = await admin.query('SELECT pg_has_role(current_user, $1, \'SET\') AS allowed', [bootstrap]);
      expect(retained.rows[0]?.allowed).toBe(false);
    } finally {
      await admin.end();
    }

    execFileSync(process.execPath, ['--conditions=development', new URL('../../../bin/gke-migrate.mjs', import.meta.url).pathname], {
      env: {
        ...process.env,
        RUNNER_MIGRATE_DATABASE_URL: url(runner, bootstrap),
        RUNNER_DB_ROLE_PREFIX: prefix,
        RUNNER_DB_API_LOGIN: api,
        RUNNER_DB_VAULT_LOGIN: vault,
        RUNNER_DB_CONNECTION_LIMITS: '{"api":2,"vault":1,"bootstrap":2}',
        RUNNER_DB_CORE_CONNECTIONS: '16',
        RUNNER_DB_CORE_NAME: core,
        RUNNER_DB_NAME: runner,
        RUNNER_INSTALL_GROUP_ID: 'rg_fixture',
        RUNNER_INSTALL_RUNNER_ID: 'runner_fixture',
        RUNNER_INSTALL_WORKER_CAPACITY: '2',
      },
      stdio: 'pipe',
    });
    process.env.RUNNER_DB_ROLE_PREFIX = prefix;
    for (const [role, login] of [['api', api], ['scheduler', api], ['vault', vault]] as const) {
      const pool = openPool(role, url(runner, login), { max: 1 });
      pools.push(pool);
      const { rows } = await pool.query('SELECT current_user AS role');
      expect(rows[0]?.role).toBe(`${prefix}_${role}`);
      if (role === 'api') {
        const enrolled = await pool.query('SELECT runner_id, group_id, capacity FROM runners');
        expect(enrolled.rows).toEqual([{ runner_id: 'runner_fixture', group_id: 'rg_fixture', capacity: 2 }]);
      }
      if (role !== 'vault') await expect(pool.query('SELECT 1 FROM vault_keys')).rejects.toMatchObject({ code: '42501' });
    }
  } finally {
    if (previousPrefix === undefined) delete process.env.RUNNER_DB_ROLE_PREFIX;
    else process.env.RUNNER_DB_ROLE_PREFIX = previousPrefix;
    await Promise.all(pools.map((pool) => pool.end()));
    for (const database of [core, runner]) await root.query(`DROP DATABASE IF EXISTS ${q(database)} WITH (FORCE)`);
    for (const role of [api, vault, ...['api', 'scheduler', 'worker', 'vault'].map((role) => `${prefix}_${role}`), bootstrap, operator]) {
      await root.query(`DROP ROLE IF EXISTS ${q(role)}`);
    }
    await root.end();
  }
});
