// CF3a: verify D1 and ExecutionDO schema parity against the PostgreSQL store manifest.
//
// 1. Applies all D1 migrations to Miniflare D1 and checks every table marked "d1"
//    in postgres-manifest.json exists with identical columns, types, and nullability.
// 2. Applies main's ExecutionDO schema (every migrations/do/*.sql, in order) and checks
//    every table marked "do" exists and carries org_id, project_id, execution_id (PRD §14).
// 3. Asserts mutual exclusion: no table is in both D1 and the DO.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { startWorker, root } from './harness.mjs';

/** D1-only columns: admission_pool.revision fences admission batches (CF3 fix 3, 1005);
 * the runbook source digests and sizes of sources kept in R2 (CF11, 1010). */
const D1_ONLY_COLUMNS = {
  admission_pool: ['revision'],
  // UI-1b (1018): the stored listing the runbook list reads instead of
  // the sources in R2. Postgres derives it while reading its draft rows.
  runbook: [
    'draft_digest', 'draft_size',
    'listing', 'listing_search', 'listing_digest', 'listing_revision', 'listing_format',
  ],
  runbook_version: ['source_digest', 'source_size'],
  draft: ['source_digest', 'source_size'],
  // CF12b review round 2 (1015): the backoff of a replay row whose R2
  // object delete failed. Postgres keeps replay responses in the row and
  // has no such sweep.
  directory_request: ['sweep_after', 'sweep_failures'],
};

let worker;
let db;
let doDb;

function splitStatements(sql) {
  const noComments = sql.replace(/--.*$/gm, '');
  const statements = [];
  let current = '';
  let inTrigger = false;
  for (const line of noComments.split('\n')) {
    current += line + '\n';
    const trimmed = line.trim().toUpperCase();
    if (trimmed.startsWith('CREATE TRIGGER')) {
      inTrigger = true;
    }
    if (inTrigger) {
      if (trimmed === 'END;' || trimmed.endsWith('END;')) {
        inTrigger = false;
        statements.push(current.trim());
        current = '';
      }
    } else if (trimmed.endsWith(';')) {
      statements.push(current.trim());
      current = '';
    }
  }
  if (current.trim()) {
    statements.push(current.trim());
  }
  return statements.filter((s) => s.length > 0);
}

before(async () => {
  worker = await startWorker({ faults: false });
  db = await worker.mf.getD1Database('DB');

  // Apply D1 migrations to Miniflare D1
  const d1Dir = join(root, 'migrations', 'd1');
  const files = readdirSync(d1Dir)
    .filter((f) => f.endsWith('.sql'))
    .sort();

  for (const file of files) {
    const sql = readFileSync(join(d1Dir, file), 'utf8');
    const stmts = splitStatements(sql);
    for (const stmt of stmts) {
      await db.prepare(stmt).run();
    }
  }

  // Apply main's ExecutionDO schema to an in-memory SQLite database
  doDb = new DatabaseSync(':memory:');
  // Every DO migration, in order (0001_execution, 0002_session_fence, ...).
  const doDir = join(root, 'migrations', 'do');
  for (const file of readdirSync(doDir).filter((f) => f.endsWith('.sql')).sort()) {
    doDb.exec(readFileSync(join(doDir, file), 'utf8'));
  }
});

after(async () => {
  doDb?.close();
  await worker?.dispose();
});

test('D1 schema migrations apply and match the PostgreSQL manifest exactly', async () => {
  const manifestPath = join(root, 'migrations', 'postgres-manifest.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));

  const d1Tables = manifest.tables.filter((t) => t.target === 'd1');
  assert.equal(d1Tables.length, 61, 'expected exactly 61 D1 tables in manifest');

  for (const table of d1Tables) {
    const info = await db.prepare(`PRAGMA table_info(${table.name})`).all();
    assert.ok(
      info.results && info.results.length > 0,
      `Table ${table.name} must exist in D1`,
    );

    const d1Cols = new Map(info.results.map((c) => [c.name, c]));
    const manifestColNames = new Set(table.columns.map((c) => c.name));

    // Verify all manifest columns exist with matching type and nullability
    for (const col of table.columns) {
      const d1Col = d1Cols.get(col.name);
      assert.ok(
        d1Col,
        `Table ${table.name} is missing expected column '${col.name}'`,
      );

      assert.equal(
        d1Col.type.toUpperCase(),
        col.d1_type.toUpperCase(),
        `Table ${table.name}, column '${col.name}' type mismatch: expected ${col.d1_type}, got ${d1Col.type}`,
      );

      const isNullable = d1Col.notnull === 0 && d1Col.pk === 0;
      assert.equal(
        isNullable,
        col.nullable,
        `Table ${table.name}, column '${col.name}' nullability mismatch: expected nullable=${col.nullable}, got nullable=${isNullable}`,
      );
    }

    // Verify no unexpected extra columns exist in D1 table. D1-only
    // migrations (1001+) may add columns Postgres does not need; each is
    // listed here with its reason.
    for (const [colName] of d1Cols) {
      if ((D1_ONLY_COLUMNS[table.name] ?? []).includes(colName)) continue;
      assert.ok(
        manifestColNames.has(colName),
        `Table ${table.name} has unexpected extra column '${colName}' in D1`,
      );
    }
  }
});

test('ExecutionDO schema matches PostgreSQL manifest and enforces PRD §14 run-owned scoping', async () => {
  const manifestPath = join(root, 'migrations', 'postgres-manifest.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));

  const doTables = manifest.tables.filter((t) => t.target === 'do');
  assert.equal(doTables.length, 14, 'expected exactly 14 DO tables in manifest');

  for (const table of doTables) {
    const targetTable = table.target_table || table.name;
    const info = doDb.prepare(`PRAGMA table_info(${targetTable})`).all();
    assert.ok(
      info && info.length > 0,
      `ExecutionDO table '${targetTable}' (for Postgres '${table.name}') must exist in DO schema`,
    );

    const doCols = new Map(info.map((c) => [c.name, c]));

    // PRD §14: Every run-owned row in DO carries org_id, project_id, and execution_id
    assert.ok(
      doCols.has('org_id'),
      `DO table '${targetTable}' must have 'org_id' column for PRD §14 run scoping`,
    );
    assert.ok(
      doCols.has('project_id'),
      `DO table '${targetTable}' must have 'project_id' column for PRD §14 run scoping`,
    );
    assert.ok(
      doCols.has('execution_id'),
      `DO table '${targetTable}' must have 'execution_id' column for PRD §14 run scoping`,
    );
  }
});

test('no table is present in both D1 and the ExecutionDO (mutual exclusion)', async () => {
  const d1Info = await db
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
    .all();
  const d1Names = (d1Info.results || [])
    .map((r) => r.name)
    .filter((n) => !n.startsWith('_cf_') && !n.endsWith('_fts') && !n.includes('_fts_'));

  const doRows = doDb
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
    .all();
  const doNames = doRows
    .map((r) => r.name)
    .filter((n) => !n.startsWith('sqlite_'));

  const overlap = d1Names.filter((name) => doNames.includes(name));
  assert.deepEqual(
    overlap,
    [],
    `Tables found in both D1 and DO: ${overlap.join(', ')}`,
  );
});

test('FTS5 virtual tables and triggers exist for search-enabled tables', async () => {
  // run_summary_fts
  const runSummaryFts = await db
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'run_summary_fts'")
    .first();
  assert.ok(runSummaryFts, 'run_summary_fts virtual table must exist');

  // memory_entry_fts
  const memoryEntryFts = await db
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'memory_entry_fts'")
    .first();
  assert.ok(memoryEntryFts, 'memory_entry_fts virtual table must exist');
});

test('CF6: the OrgDO schema matches the PostgreSQL manifest (0030_quotas_usage) exactly', () => {
  const manifest = JSON.parse(readFileSync(join(root, 'migrations', 'postgres-manifest.json'), 'utf8'));
  const orgTables = manifest.tables.filter((t) => t.target === 'orgdo');
  assert.deepEqual(orgTables.map((t) => t.name).sort(), ['quota_reservation', 'usage_monthly']);
  const orgDb = new DatabaseSync(':memory:');
  try {
    const orgDir = join(root, 'migrations', 'orgdo');
    for (const file of readdirSync(orgDir).filter((f) => f.endsWith('.sql')).sort()) {
      orgDb.exec(readFileSync(join(orgDir, file), 'utf8'));
    }
    for (const table of orgTables) {
      const info = orgDb.prepare(`PRAGMA table_info(${table.target_table || table.name})`).all();
      const columns = new Map(info.map((c) => [c.name, c]));
      assert.deepEqual([...columns.keys()].sort(), table.columns.map((c) => c.name).sort(), table.name);
      for (const column of table.columns) {
        const actual = columns.get(column.name);
        assert.equal(actual.type.toUpperCase(), column.d1_type.toUpperCase(), `${table.name}.${column.name}`);
        assert.equal(actual.notnull === 0 && actual.pk === 0, column.nullable, `${table.name}.${column.name}`);
      }
    }
  } finally {
    orgDb.close();
  }
});

test('CF11: the WebhookDO schema holds the delivery log with the PostgreSQL columns exactly', () => {
  const manifest = JSON.parse(readFileSync(join(root, 'migrations', 'postgres-manifest.json'), 'utf8'));
  const hookTables = manifest.tables.filter((t) => t.target === 'webhookdo');
  assert.deepEqual(hookTables.map((t) => t.name).sort(), ['webhook_attempt', 'webhook_delivery']);
  const hookDb = new DatabaseSync(':memory:');
  try {
    const hookDir = join(root, 'migrations', 'webhookdo');
    for (const file of readdirSync(hookDir).filter((f) => f.endsWith('.sql')).sort()) {
      hookDb.exec(readFileSync(join(hookDir, file), 'utf8'));
    }
    for (const table of hookTables) {
      const info = hookDb.prepare(`PRAGMA table_info(${table.target_table || table.name})`).all();
      const columns = new Map(info.map((c) => [c.name, c]));
      assert.deepEqual([...columns.keys()].sort(), table.columns.map((c) => c.name).sort(), table.name);
      for (const column of table.columns) {
        const actual = columns.get(column.name);
        assert.equal(actual.type.toUpperCase(), column.d1_type.toUpperCase(), `${table.name}.${column.name}`);
        assert.equal(actual.notnull === 0 && actual.pk === 0, column.nullable, `${table.name}.${column.name}`);
      }
    }
  } finally {
    hookDb.close();
  }
});

test('CF11: the delivery log is not in D1 any more', async () => {
  for (const table of ['webhook_delivery', 'webhook_attempt']) {
    const found = await db.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?").bind(table).first();
    assert.equal(found, null, `${table} is dropped from D1 (1009)`);
  }
});
