// The runner's own Postgres database (design §5.3, §7.9): one database role
// per process, each connecting with its own login. `openPool(role, url)`
// checks on every new connection that the login is a member of that role's
// group and of no other process role's group, is not a superuser and owns
// nothing in the runner schema, so a misconfigured URL fails closed.
import { createHash } from 'node:crypto';
import { readdir, readFile } from 'node:fs/promises';
import pg from 'pg';

export type DbRole = 'api' | 'scheduler' | 'worker' | 'vault';
export const DB_ROLES: readonly DbRole[] = ['api', 'scheduler', 'worker', 'vault'];
export const ROLE_NAMES: Record<DbRole, string> = {
  api: 'rb_api',
  scheduler: 'rb_scheduler',
  worker: 'rb_worker',
  vault: 'rb_vault',
};

export class DbRoleError extends Error {
  constructor(role: DbRole, why: string) {
    super(`database login for ${ROLE_NAMES[role]} ${why}`);
    this.name = 'DbRoleError';
  }
}

export function openPool(role: DbRole, url: string, options: { max?: number } = {}): pg.Pool {
  const group = ROLE_NAMES[role];
  const pool = new pg.Pool({
    connectionString: url,
    max: options.max ?? 10,
    application_name: `runbook-runner-${role}`,
    idleTimeoutMillis: 30_000,
    // Runs on every new connection; a rejection fails the connect (and so the query).
    onConnect: async (client) => {
      const others = DB_ROLES.filter((r) => r !== role).map((r) => ROLE_NAMES[r]);
      const { rows } = await client.query<{ member: boolean; superuser: boolean; also: string[]; owned: number }>(
        `SELECT pg_has_role(current_user, $1, 'MEMBER') AS member,
                (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) AS superuser,
                ARRAY(SELECT o FROM unnest($2::text[]) AS o
                      WHERE EXISTS (SELECT FROM pg_roles WHERE rolname = o) AND pg_has_role(current_user, o, 'MEMBER')) AS also,
                (SELECT count(*)::int FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                  WHERE n.nspname = current_schema() AND pg_has_role(current_user, c.relowner, 'MEMBER')) AS owned`,
        [group, others],
      );
      const row = rows[0];
      if (!row?.member) throw new DbRoleError(role, `is not a member of ${group}`);
      if (row.superuser) throw new DbRoleError(role, `must not be a superuser (use a member of ${group})`);
      if (row.also.length > 0) throw new DbRoleError(role, `must not also be a member of ${row.also.join(', ')}`);
      if (row.owned > 0) throw new DbRoleError(role, `owns objects in the runner schema (use a login that only has ${group}'s grants)`);
    },
  });
  // An idle client's network error must not crash the process.
  pool.on('error', () => undefined);
  return pool;
}

/** Run `fn` in one transaction on one connection. */
export async function withTransaction<T>(pool: pg.Pool, fn: (client: pg.PoolClient) => Promise<T>): Promise<T> {
  const client = await pool.connect();
  try {
    await client.query('BEGIN');
    const result = await fn(client);
    await client.query('COMMIT');
    return result;
  } catch (e) {
    await client.query('ROLLBACK').catch(() => undefined);
    throw e;
  } finally {
    client.release();
  }
}

export const MIGRATIONS_DIR = new URL('../../api/migrations/', import.meta.url);
const MIGRATE_LOCK = 7419002;

/**
 * Apply pending migrations, in name order, each in its own transaction, under
 * an advisory lock so replicas starting together are safe. Forward-only and
 * checksummed: an applied file that changed is an error.
 */
export async function migrate(ownerUrl: string, dir: URL = MIGRATIONS_DIR): Promise<string[]> {
  const files = (await readdir(dir)).filter((f) => /^\d{4}_[a-z0-9_]+\.sql$/.test(f)).sort();
  const client = new pg.Client({ connectionString: ownerUrl, application_name: 'runbook-runner-migrate' });
  await client.connect();
  const applied: string[] = [];
  try {
    await client.query('SELECT pg_advisory_lock($1)', [MIGRATE_LOCK]);
    await client.query(`CREATE TABLE IF NOT EXISTS runner_schema_migrations (
      version text PRIMARY KEY, checksum text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())`);
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
        await client.query(sql);
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
