// The image's migration creates the NOLOGIN roles. Then the Compose owner
// provisions one separate login per role and seeds the registered placement.
import { createRequire } from 'node:module';
import { loadMigrationConfig } from './migration-config.mjs';
import { migrate } from '/app/packages/shared/dist/db.js';

const { Client } = createRequire('/app/package.json')('pg');
const { url, group, runner, capacity, version, passwords } = loadMigrationConfig();

const db = new Client({ connectionString: url });
try {
  await migrate(url);
  await db.connect();
  await db.query('BEGIN');
  await db.query('SELECT pg_advisory_xact_lock(7419003)');
  for (const [role, password] of Object.entries(passwords)) {
    const user = `runner_${role}`;
    const exists = await db.query('SELECT 1 FROM pg_roles WHERE rolname = $1', [user]);
    if (!exists.rowCount) await db.query(`CREATE ROLE ${user} LOGIN PASSWORD '${password}'`);
    await db.query(`GRANT rb_${role} TO ${user}`);
  }
  await db.query("INSERT INTO groups(group_id, name) VALUES ($1, 'default') ON CONFLICT (group_id) DO NOTHING", [group]);
  await db.query(
    "INSERT INTO runners(runner_id, group_id, name, capacity, version, status) VALUES ($1, $2, 'docker', $3, $4, 'ready') ON CONFLICT (runner_id) DO UPDATE SET capacity = EXCLUDED.capacity, version = EXCLUDED.version",
    [runner, group, capacity, version],
  );
  await db.query('COMMIT');
} catch {
  // SQL errors from CREATE ROLE can contain credential material.
  console.error('Runner database initialization failed');
  process.exitCode = 1;
} finally {
  await db.end();
}
