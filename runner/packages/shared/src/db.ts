// The runner's own Postgres database (design §5.3, §7.9): one database role
// per process, each connecting with its own login. `openPool(role, url)`
// checks on every new connection that the login is a member of that role's
// group and of no other process role's group, is not a superuser and owns
// nothing in the runner schema, so a misconfigured URL fails closed.
import { createHash } from 'node:crypto';
import { readdir, readFile } from 'node:fs/promises';
import { setTimeout as sleep } from 'node:timers/promises';
import pg from 'pg';
import { createLogger, errorFields } from './log.ts';

export type DbRole = 'api' | 'scheduler' | 'worker' | 'vault';
export const DB_ROLES: readonly DbRole[] = ['api', 'scheduler', 'worker', 'vault'];
export const ROLE_NAMES: Record<DbRole, string> = {
  api: 'rb_api',
  scheduler: 'rb_scheduler',
  worker: 'rb_worker',
  vault: 'rb_vault',
};

export function installRoleNames(prefix: string): typeof ROLE_NAMES {
  if (!/^[a-z][a-z0-9_]{2,29}$/.test(prefix)) throw new Error('invalid RUNNER_DB_ROLE_PREFIX');
  return { api: `${prefix}_api`, scheduler: `${prefix}_scheduler`, worker: `${prefix}_worker`, vault: `${prefix}_vault` };
}

export function sqlIdentifier(name: string): string {
  return '"' + name.replaceAll('"', '""') + '"';
}

export class DbRoleError extends Error {
  constructor(role: DbRole, why: string) {
    super(`database login for ${ROLE_NAMES[role]} ${why}`);
    this.name = 'DbRoleError';
  }
}

export function openPool(role: DbRole, url: string, options: { max?: number } = {}): pg.Pool {
  const prefix = process.env.RUNNER_DB_ROLE_PREFIX;
  const names = prefix ? installRoleNames(prefix) : ROLE_NAMES;
  const group = names[role];
  const max = options.max ?? Number(process.env.RUNNER_DB_POOL_SIZE ?? 4);
  if (!Number.isSafeInteger(max) || max < 1 || max > 10) throw new DbRoleError(role, 'has an invalid pool size');
  const log = createLogger(role);
  let retryAt = 0;
  let consecutiveFailures = 0;
  let nextLogAt = 0;
  let failuresSinceLog = 0;
  const report = (phase: string, error: unknown) => {
    failuresSinceLog++;
    if (Date.now() < nextLogAt) return;
    log.warn('database pool error', {
      phase, ...errorFields(error), max, open: pool.totalCount, idle: pool.idleCount,
      waiting: pool.waitingCount, failures: failuresSinceLog,
    });
    failuresSinceLog = 0;
    nextLogAt = Date.now() + 5_000;
  };
  // pg-pool calls the callback once for each physical connection attempt.
  // Failures slow future attempts across the pool without replaying SQL.
  class BackoffClient extends pg.Client {
    override connect(): Promise<pg.Client>;
    override connect(callback: ((error: Error) => void) | ((error: null, client: pg.Client) => void)): void;
    override connect(callback?: ((error: Error) => void) | ((error: null, client: pg.Client) => void)): Promise<pg.Client> | void {
      const attempt = async () => {
        const delay = retryAt - performance.now();
        if (delay > 0) await sleep(delay + Math.random() * 200);
        try {
          const client = await super.connect();
          retryAt = 0;
          consecutiveFailures = 0;
          return client;
        } catch (error) {
          const limit = Math.min(1_500, 250 * 2 ** consecutiveFailures);
          consecutiveFailures = Math.min(4, consecutiveFailures + 1);
          retryAt = performance.now() + limit * (0.5 + Math.random() * 0.5);
          report('connect', error);
          throw error;
        }
      };
      if (!callback) return attempt();
      // pg-pool passes a single Node-style callback for both outcomes.
      const done = callback as (error: Error | null, client?: pg.Client) => void;
      void attempt().then((client) => done(null, client), (error: Error) => done(error));
    }
  }
  // Bound each of the role checks, including when a proxy accepted the
  // session but stopped forwarding queries. pg supports this per-query option.
  const roleCheck = (text: string) => ({ text, query_timeout: 1_000 });
  const pool = new pg.Pool({
    Client: BackoffClient,
    connectionString: url,
    max,
    connectionTimeoutMillis: 4_000,
    idle_in_transaction_session_timeout: 15_000,
    application_name: `runbook-runner-${role}`,
    idleTimeoutMillis: 30_000,
    // Runs on every new connection; a rejection fails the connect (and so the query).
    onConnect: async (client) => {
      // The IAM login may switch roles, so inspect its privileges first.
      const allowed = prefix && (role === 'api' || role === 'scheduler')
        ? [names.api, names.scheduler] : [group];
      const { rows: [row] } = await client.query<{ memberships: string[]; privileged: boolean; owned: boolean }>(
        roleCheck(`SELECT ARRAY(SELECT r.rolname::text FROM pg_roles r WHERE r.rolname <> session_user
                       AND pg_has_role(session_user, r.oid, 'MEMBER')) AS memberships,
                EXISTS (SELECT FROM pg_roles r WHERE pg_has_role(session_user, r.oid, 'MEMBER')
                        AND (r.rolsuper OR r.rolcreaterole OR r.rolcreatedb OR r.rolreplication OR r.rolbypassrls)) AS privileged,
                EXISTS (SELECT FROM pg_shdepend d CROSS JOIN pg_database db
                        WHERE db.datname = current_database() AND d.refclassid = 'pg_authid'::regclass
                          AND (d.dbid = db.oid OR
                               (d.dbid = 0 AND d.classid = 'pg_database'::regclass AND d.objid = db.oid))
                          AND d.deptype = 'o' AND pg_has_role(session_user, d.refobjid, 'MEMBER')) AS owned
         FROM pg_roles WHERE rolname = session_user`),
      );
      if (row?.privileged) throw new DbRoleError(role, 'must not have privileged role attributes');
      if (!row || !allowed.every((name) => row.memberships.includes(name))) {
        throw new DbRoleError(role, `is not a member of ${allowed.join(', ')}`);
      }
      // Cloud SQL assigns this login marker to service accounts.
      const other = row.memberships.filter((name) => !allowed.includes(name)
        && !(prefix && name === 'cloudsqliamserviceaccount'));
      if (other.length) throw new DbRoleError(role, `must not also be a member of ${other.join(', ')}`);
      if (row.owned) throw new DbRoleError(role, 'owns objects in the runner database');
      if (prefix) {
        await client.query(roleCheck(`SET ROLE ${sqlIdentifier(group)}`));
        const { rows: [effective] } = await client.query<{ role: string }>(roleCheck('SELECT current_user AS role'));
        if (effective?.role !== group) throw new DbRoleError(role, `did not assume ${group}`);
      }
    },
  });
  // pg-pool removes its own listener while the client is checked out.
  pool.on('connect', (client) => client.on('error', (error) => report('session', error)));
  pool.on('error', () => undefined);
  log.info('database pool configured', { max, acquire_timeout_ms: 4_000 });
  return pool;
}

/** Run `fn` in one transaction on one connection. */
export async function withTransaction<T>(pool: pg.Pool, fn: (client: pg.PoolClient) => Promise<T>): Promise<T> {
  const client = await pool.connect();
  let discard = false;
  try {
    await client.query('BEGIN');
    const result = await fn(client);
    await client.query('COMMIT');
    return result;
  } catch (e) {
    try {
      await client.query('ROLLBACK');
    } catch {
      discard = true;
    }
    throw e;
  } finally {
    client.release(discard);
  }
}

export const MIGRATIONS_DIR = new URL('../../api/migrations/', import.meta.url);
const MIGRATE_LOCK = 7419002;

/**
 * Apply pending migrations, in name order, each in its own transaction, under
 * an advisory lock so replicas starting together are safe. Forward-only and
 * checksummed: an applied file that changed is an error.
 */
export async function migrate(ownerUrl: string, dir: URL = MIGRATIONS_DIR, rolePrefix?: string): Promise<string[]> {
  const names = rolePrefix ? installRoleNames(rolePrefix) : ROLE_NAMES;
  const files = (await readdir(dir)).filter((f) => /^\d{4}_[a-z0-9_]+\.sql$/.test(f)).sort();
  const client = new pg.Client({ connectionString: ownerUrl, application_name: 'runbook-runner-migrate', connectionTimeoutMillis: 4_000 });
  await client.connect();
  const applied: string[] = [];
  try {
    await client.query('SELECT pg_advisory_lock($1)', [MIGRATE_LOCK]);
    await client.query(`CREATE TABLE IF NOT EXISTS runner_schema_migrations (
      version text PRIMARY KEY, checksum text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())`);
    if (rolePrefix) {
      await client.query('BEGIN');
      try {
        await client.query('CREATE TABLE IF NOT EXISTS runner_role_namespace (singleton boolean PRIMARY KEY CHECK (singleton), prefix text NOT NULL)');
        const { rows: [current] } = await client.query<{ prefix: string }>('SELECT prefix FROM runner_role_namespace');
        if (current && current.prefix !== rolePrefix) throw new Error('runner database role prefix changed');
        if (!current) {
          const { rowCount } = await client.query('SELECT FROM pg_roles WHERE rolname = ANY($1::text[])', [Object.values(names)]);
          const { rowCount: migrations } = await client.query('SELECT FROM runner_schema_migrations LIMIT 1');
          if (rowCount || migrations) throw new Error('runner database role prefix collides with an existing installation');
          await client.query('INSERT INTO runner_role_namespace VALUES (true, $1)', [rolePrefix]);
        }
        await client.query('COMMIT');
      } catch (error) {
        await client.query('ROLLBACK');
        throw error;
      }
    }
    const done = new Map(
      (await client.query<{ version: string; checksum: string }>('SELECT version, checksum FROM runner_schema_migrations'))
        .rows.map((r) => [r.version, r.checksum]),
    );
    for (const file of files) {
      const version = file.replace(/\.sql$/, '');
      const sql = await readFile(new URL(file, dir), 'utf8');
      const checksum = createHash('sha256').update(sql).digest('hex');
      const previous = done.get(version);
      if (previous !== undefined) {
        if (previous !== checksum) throw new Error(`migration ${version} changed after it was applied`);
        continue;
      }
      await client.query('BEGIN');
      try {
        const installSql = rolePrefix
          ? sql.replace(/\brb_(api|scheduler|worker|vault)\b/g, (_match, role: DbRole) => names[role])
          : sql;
        await client.query(installSql);
        await client.query('INSERT INTO runner_schema_migrations (version, checksum) VALUES ($1, $2)', [
          version,
          checksum,
        ]);
        await client.query('COMMIT');
      } catch (e) {
        await client.query('ROLLBACK').catch(() => undefined);
        throw e;
      }
      applied.push(version);
    }
  } finally {
    await client.query('SELECT pg_advisory_unlock($1)', [MIGRATE_LOCK]).catch(() => undefined);
    await client.end();
  }
  return applied;
}
