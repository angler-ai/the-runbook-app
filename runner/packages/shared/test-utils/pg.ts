// Throwaway Postgres databases for tests. RUNNER_TEST_DATABASE_URL names a
// superuser connection to a disposable server (for example
// `docker run -d -e POSTGRES_PASSWORD=pg -p 55433:5432 postgres:16` and
// postgres://postgres:pg@localhost:55433/postgres). Each test file gets its
// own database with the migrations applied, and one login role per process
// role (rb_api, rb_scheduler, rb_worker, rb_vault) to connect as.
import { randomBytes } from 'node:crypto';
import pg from 'pg';
import { type DbRole, DB_ROLES, migrate, openPool, ROLE_NAMES } from '../src/db.ts';

export const TEST_DATABASE_URL = process.env.RUNNER_TEST_DATABASE_URL;
/** Use as `describe.skipIf(!hasTestDatabase)`. */
export const hasTestDatabase = Boolean(TEST_DATABASE_URL);

/** Obviously fake; the roles exist only on a throwaway test server. */
const LOGIN_PASSWORD = 'fake-test-only-password';
export const loginRole = (role: DbRole) => `${ROLE_NAMES[role]}_test_login`;

function adminUrl(database?: string): string {
  if (!TEST_DATABASE_URL) throw new Error('RUNNER_TEST_DATABASE_URL is not set');
  const u = new URL(TEST_DATABASE_URL);
  if (database) u.pathname = `/${database}`;
  return u.toString();
}

/** Create the group roles and one login role per process role, once per server. */
export async function ensureTestRoles(): Promise<void> {
  const client = new pg.Client({ connectionString: adminUrl() });
  await client.connect();
  try {
    await client.query('SELECT pg_advisory_lock(7419001)');
    for (const role of DB_ROLES) {
      const group = ROLE_NAMES[role];
      const login = loginRole(role);
      const exists = async (name: string) =>
        (await client.query('SELECT 1 FROM pg_roles WHERE rolname = $1', [name])).rowCount === 1;
      if (!(await exists(group))) await client.query(`CREATE ROLE ${group} NOLOGIN`);
      if (!(await exists(login))) {
        await client.query(`CREATE ROLE ${login} LOGIN PASSWORD '${LOGIN_PASSWORD}' IN ROLE ${group}`);
      }
    }
  } finally {
    await client.query('SELECT pg_advisory_unlock(7419001)').catch(() => undefined);
    await client.end();
  }
}

export interface TestDatabase {
  name: string;
  /** Superuser URL for this database (migrations, tampering in tests). */
  adminUrl: string;
  /** Login URL for a process role. */
  urlFor(role: DbRole): string;
  /** A pool connected as a process role (closed by `drop`). */
  pool(role: DbRole): pg.Pool;
  /** A superuser pool for this database (closed by `drop`). */
  admin(): pg.Pool;
  drop(): Promise<void>;
}

export async function createTestDatabase(
  options: { beforeMigrate?: (admin: pg.Client) => Promise<unknown> } = {},
): Promise<TestDatabase> {
  await ensureTestRoles();
  const name = `rb_test_${randomBytes(6).toString('hex')}`;
  const root = new pg.Client({ connectionString: adminUrl() });
  await root.connect();
  await root.query(`CREATE DATABASE ${name}`);
  await root.end();
  const dbAdminUrl = adminUrl(name);
  if (options.beforeMigrate) {
    const c = new pg.Client({ connectionString: dbAdminUrl });
    await c.connect();
    try {
      await options.beforeMigrate(c);
    } finally {
      await c.end();
    }
  }
  await migrate(dbAdminUrl);
  const pools: pg.Pool[] = [];
  const urlFor = (role: DbRole) => {
    const u = new URL(dbAdminUrl);
    u.username = loginRole(role);
    u.password = LOGIN_PASSWORD;
    return u.toString();
  };
  return {
    name,
    adminUrl: dbAdminUrl,
    urlFor,
    pool(role) {
      const p = openPool(role, urlFor(role), { max: 4 });
      pools.push(p);
      return p;
    },
    admin() {
      const p = new pg.Pool({ connectionString: dbAdminUrl, max: 2 });
      // DROP DATABASE ... WITH (FORCE) terminates idle connections (57P01).
      // Without a listener the pool re-emits that as an unhandled error.
      // Query errors still reject their own promises.
      p.on('error', () => undefined);
      pools.push(p);
      return p;
    },
    async drop() {
      await Promise.all(pools.map((p) => p.end().catch(() => undefined)));
      const c = new pg.Client({ connectionString: adminUrl() });
      await c.connect();
      await c.query(`DROP DATABASE IF EXISTS ${name} WITH (FORCE)`);
      await c.end();
    },
  };
}
