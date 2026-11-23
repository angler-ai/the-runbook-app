import { createLogger, errorFields, initLogRedaction } from '@runbook/runner-shared/log';
import pg from 'pg';
import { installRoleNames, migrate, sqlIdentifier } from '@runbook/runner-shared/db';

const required = (name) => {
  if (!process.env[name]) throw new Error(`missing ${name}`);
  return process.env[name];
};

const log = createLogger('gke-migrate');

async function main() {
  await initLogRedaction();
  const url = required('RUNNER_MIGRATE_DATABASE_URL');
  const coreDatabase = required('RUNNER_DB_CORE_NAME');
  const runnerDatabase = required('RUNNER_DB_NAME');
  const prefix = required('RUNNER_DB_ROLE_PREFIX');
  const names = installRoleNames(prefix);
  const api = required('RUNNER_DB_API_LOGIN');
  const vault = required('RUNNER_DB_VAULT_LOGIN');
  const limits = JSON.parse(required('RUNNER_DB_CONNECTION_LIMITS'));
  const coreConnections = Number(required('RUNNER_DB_CORE_CONNECTIONS'));
  if (![limits.api, limits.vault, limits.bootstrap, coreConnections].every((n) => Number.isSafeInteger(n) && n >= 1)) {
    throw new Error('invalid runner database connection budget');
  }
  const group = required('RUNNER_INSTALL_GROUP_ID');
  const runner = required('RUNNER_INSTALL_RUNNER_ID');
  const capacity = Number(required('RUNNER_INSTALL_WORKER_CAPACITY'));
  if (!Number.isSafeInteger(capacity) || capacity < 1) throw new Error('invalid RUNNER_INSTALL_WORKER_CAPACITY');
  const db = new pg.Client({ connectionString: url, connectionTimeoutMillis: 10_000 });
  await db.connect();
  try {
    const { rows: [status] } = await db.query(`
      SELECT current_database() AS database,
        has_database_privilege($1, $3, 'CONNECT') AS api_core,
        has_database_privilege($2, $3, 'CONNECT') AS vault_core,
        current_setting('max_connections')::int - current_setting('superuser_reserved_connections')::int
          - current_setting('reserved_connections')::int AS available,
        (SELECT count(*)::int FROM pg_roles
          WHERE (rolname, rolconnlimit) IN (($1, $4), ($2, $5), (session_user, $6))
            AND rolcanlogin AND NOT rolsuper
            AND 'idle_in_transaction_session_timeout=15s' = ANY(rolconfig)) AS limited
    `, [api, vault, coreDatabase, limits.api, limits.vault, limits.bootstrap]);
    if (status.database !== runnerDatabase || status.api_core || status.vault_core) {
      throw new Error('prepare the runner database and revoke core database access before migrating');
    }
    if (status.limited !== 3 || status.available < coreConnections + limits.api + limits.vault + limits.bootstrap + 1) {
      throw new Error('apply runner_database_connection_sql and verify the shared database connection budget before migrating');
    }
    await db.query(`REVOKE CONNECT, TEMPORARY ON DATABASE ${sqlIdentifier(runnerDatabase)} FROM PUBLIC`);
    await migrate(url, undefined, prefix);
    await db.query('BEGIN');
    await db.query(`GRANT CONNECT ON DATABASE ${sqlIdentifier(runnerDatabase)} TO ${sqlIdentifier(api)}, ${sqlIdentifier(vault)}`);
    // PostgreSQL 16 role membership separates SET ROLE from inherited grants.
    await db.query(`GRANT ${sqlIdentifier(names.api)}, ${sqlIdentifier(names.scheduler)} TO ${sqlIdentifier(api)} WITH INHERIT FALSE, SET TRUE`);
    await db.query(`GRANT ${sqlIdentifier(names.vault)} TO ${sqlIdentifier(vault)} WITH INHERIT FALSE, SET TRUE`);
    await db.query('INSERT INTO groups(group_id, name) VALUES ($1, $1) ON CONFLICT (group_id) DO NOTHING', [group]);
    await db.query(`INSERT INTO runners(runner_id, group_id, name, capacity) VALUES ($1, $2, 'gke', $3)
                     ON CONFLICT (runner_id) DO UPDATE SET capacity = EXCLUDED.capacity, group_id = EXCLUDED.group_id`,
      [runner, group, capacity]);
    await db.query('COMMIT');
    log.info('runner database migrated');
  } finally {
    await db.end();
  }
}

main().catch((error) => {
  // PostgreSQL errors can include SQL text and connection details.
  log.error('runner database migration failed', errorFields(error));
  process.exitCode = 1;
});
