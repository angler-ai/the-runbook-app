// UPD1: tests for scripts/check-migrations.mjs (the expand/contract rule).
//   node --test scripts/tests/
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, readdirSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { BASELINE, DIRECTORIES, annotationFloor, check, findings, rollbackFloor, statements, workspaceVersion } from '../check-migrations.mjs';

const repo = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const rules = (sql) => findings(sql).map((f) => f.rule);

test('additive changes need no annotation', () => {
  for (const sql of [
    'CREATE TABLE runbook.audit (id TEXT PRIMARY KEY, note TEXT NOT NULL);',
    'CREATE TABLE t (x INTEGER NOT NULL);\nCREATE UNIQUE INDEX t_x ON t (x);',
    'ALTER TABLE runbook.run ADD COLUMN note TEXT;',
    'ALTER TABLE run_release ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;',
    'ALTER TABLE runbook.run ADD COLUMN IF NOT EXISTS a TEXT, ADD COLUMN b INT NOT NULL DEFAULT 1;',
    'CREATE INDEX IF NOT EXISTS run_note ON runbook.run (note);',
    "INSERT INTO settings (k, v) VALUES ('a', '-- not a comment; nor an end');",
    // A SQLite table rebuild of a table this file created.
    'CREATE TABLE x_new (a TEXT);\nALTER TABLE x_new RENAME TO x2;',
  ]) {
    assert.deepEqual(rules(sql), [], sql);
  }
  // A trigger on a table the file creates, dropped and created again
  // (SQLite cannot alter a trigger; UPD1-31: the drop is exempt because the
  // file created it).
  const lite = 'CREATE TABLE t (x INT);\nCREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO u VALUES (1); END;\n' +
    'DROP TRIGGER IF EXISTS t_ai;\nCREATE TRIGGER t_ai AFTER INSERT ON t BEGIN\n  INSERT INTO u VALUES (1);\n  INSERT INTO w VALUES (2);\nEND;';
  assert.deepEqual(findings(lite, 'sqlite').map((f) => f.rule), []);
});

test('every contract change is found', () => {
  const cases = [
    ['DROP TABLE runbook.legacy;', /DROP TABLE/],
    ['DROP INDEX IF EXISTS runbook.run_note;', /DROP INDEX/],
    ['ALTER TABLE runbook.run DROP COLUMN note;', /ALTER \.\.\. DROP/],
    ['ALTER TABLE runbook.run DROP CONSTRAINT run_check;', /ALTER \.\.\. DROP/],
    ['ALTER TABLE runbook.run RENAME COLUMN a TO b;', /RENAME/],
    ['ALTER TABLE run RENAME TO runs;', /RENAME/],
    ['ALTER TABLE runbook.draft\n    ALTER COLUMN source TYPE BYTEA USING x::bytea;', /type change/],
    ['ALTER TABLE t ALTER c SET DATA TYPE bigint;', /type change/],
    ['ALTER TABLE t ALTER COLUMN c SET NOT NULL;', /SET NOT NULL/],
    ['ALTER TABLE t ADD COLUMN c TEXT NOT NULL;', /NOT NULL without a DEFAULT/],
    ['ALTER TABLE t ADD CONSTRAINT t_c CHECK (c > 0);', /constraint/],
    ['CREATE UNIQUE INDEX t_c ON runbook.t (c);', /unique index/],
    ['TRUNCATE runbook.t;', /TRUNCATE/],
    // SQLite rebuild of an existing table: the old one is dropped.
    ['CREATE TABLE x_new (a TEXT);\nINSERT INTO x_new SELECT a FROM x;\nDROP TABLE x;\nALTER TABLE x_new RENAME TO x;', /DROP TABLE/],
  ];
  for (const [sql, rule] of cases) {
    const found = rules(sql);
    assert.equal(found.length, 1, `${sql}: ${JSON.stringify(found)}`);
    assert.match(found[0], rule, sql);
  }
});

test('statements carry the comments just above them and their line', () => {
  const parsed = statements('-- head\n\n-- contract(unused-since 0.1.0): x\nDROP TABLE a;\nCREATE TABLE b (\n  c TEXT\n);\n');
  assert.equal(parsed.length, 2);
  assert.deepEqual(parsed[0].comments, ['-- contract(unused-since 0.1.0): x']);
  assert.equal(parsed[0].line, 4);
  assert.equal(parsed[1].line, 5);
  assert.deepEqual(parsed[1].comments, []);
});

function fixture(files) {
  const root = mkdtempSync(join(tmpdir(), 'upd1-migrations-'));
  for (const dir of DIRECTORIES) mkdirSync(join(root, dir), { recursive: true });
  for (const [path, sql] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), sql);
  }
  return root;
}

test('a contract change needs an annotation naming an older release', () => {
  const pg = 'crates/runbook-store-postgres/migrations';
  const run = (sql, version = '0.4.0') => {
    const root = fixture({ [`${pg}/0031_x.sql`]: sql });
    try {
      return check({ root, version, baseline: {} });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  };
  assert.match(run('ALTER TABLE runbook.run DROP COLUMN note;').join(), /0031_x\.sql:1: ALTER \.\.\. DROP/);
  assert.deepEqual(
    run('-- contract(unused-since 0.3.0): 0.3.0 stopped reading run.note\nALTER TABLE runbook.run DROP COLUMN note;'),
    [],
  );
  assert.match(
    run('-- contract(unused-since 0.4.0): same release\nALTER TABLE runbook.run DROP COLUMN note;').join(),
    /must ship before the one that removes it/,
  );
  assert.match(
    run('-- contract(unused-since 1.0.0): a later release\nALTER TABLE runbook.run DROP COLUMN note;').join(),
    /must ship before/,
  );
  // Near misses of the one grammar are malformed annotations (re-check #5).
  assert.match(run('-- contract(unused-since soon): x\nDROP TABLE runbook.t;').join(), /:1: malformed annotation/);
  assert.match(run('-- contract(unused-since 0.1.0):\nDROP TABLE runbook.t;').join(), /:1: malformed annotation/);
  // The annotation covers only the statement right below it.
  assert.equal(
    run('-- contract(unused-since 0.3.0): x\nDROP TABLE runbook.a;\n\nDROP TABLE runbook.b;').length,
    1,
  );
  assert.deepEqual(run('CREATE TABLE runbook.ok (id TEXT);'), []);
});

test('D1 and Durable Object migrations are checked too; baseline files are not', () => {
  const root = fixture({
    'cloudflare/migrations/d1/1015_x.sql': 'ALTER TABLE run RENAME COLUMN a TO b;',
    'cloudflare/migrations/do/0006_x.sql': 'DROP TABLE step_run;',
    'cloudflare/migrations/orgdo/0002_x.sql': 'ALTER TABLE quota ADD COLUMN n INTEGER NOT NULL;',
    'cloudflare/migrations/webhookdo/0003_x.sql': 'CREATE TABLE ok (id TEXT);',
    'cloudflare/migrations/d1/0004_old.sql': 'DROP TABLE project_member;',
  });
  try {
    const errors = check({
      root,
      version: '0.2.0',
      baseline: { 'cloudflare/migrations/d1': ['0004_old.sql'] },
    });
    assert.equal(errors.length, 3, errors.join('\n'));
    assert.match(errors.join(), /d1\/1015_x\.sql/);
    assert.match(errors.join(), /do\/0006_x\.sql/);
    assert.match(errors.join(), /orgdo\/0002_x\.sql/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('the repository passes, and the rule would have caught the contract migrations it grandfathers', () => {
  const version = workspaceVersion(repo);
  assert.deepEqual(check({ root: repo, version }), []);
  // Every grandfathered file exists (files added since are checked above)...
  for (const dir of DIRECTORIES) {
    const names = readdirSync(join(repo, dir)).filter((n) => n.endsWith('.sql'));
    assert.deepEqual(BASELINE[dir].filter((n) => !names.includes(n)), [], `${dir}: baseline names a missing file`);
  }
  // ...and without it, the known pre-release contract changes are found.
  const errors = check({ root: repo, version, baseline: {} }).join('\n');
  assert.match(errors, /runbook-store-postgres\/migrations\/0015_draft_source_bytes\.sql:\d+: a column type change/);
  assert.match(errors, /cloudflare\/migrations\/d1\/0004_directory_member_org\.sql:\d+: DROP TABLE/);
  assert.match(errors, /cloudflare\/migrations\/d1\/1009_webhook_log_to_objects\.sql/);
});

// --- review findings (UPD1 data-safety review): each of these passed the
// first version of the check.

const flagged = (sql) => findings(sql).map((f) => `${f.kind}: ${f.rule}`);

test('a block comment in front of a statement hides nothing', () => {
  assert.match(flagged('/* x */ DROP TABLE runbook.t;').join(), /^contract: DROP TABLE/);
  assert.match(flagged('/* a /* nested */ b */\nALTER TABLE runbook.t DROP COLUMN c;').join(), /ALTER/);
});

test('two statements on one line are two statements; an annotation covers only the first', () => {
  const found = findings('-- contract(unused-since 0.1.0): x\nSELECT 1; DROP TABLE runbook.t;');
  assert.equal(found.length, 1);
  assert.match(found[0].rule, /DROP TABLE/);
  assert.deepEqual(found[0].comments, []);
});

test('dollar-quoted bodies are one statement, and DO / EXECUTE / functions cannot be checked', () => {
  // Dynamic SQL can do anything: a contract change (re-check #3).
  const doBlock = flagged('DO $$ BEGIN ALTER TABLE runbook.t DROP COLUMN c; END $$;');
  assert.equal(doBlock.length, 1);
  assert.match(doBlock[0], /^contract: DO/);
  assert.match(flagged("DO $tag$ BEGIN EXECUTE 'x'; END $tag$ LANGUAGE plpgsql;").join(), /^contract/);
  assert.match(flagged("EXECUTE 'DROP TABLE runbook.t';").join(), /^contract/);
  assert.match(flagged('CREATE FUNCTION f() RETURNS void AS $$ DROP TABLE t; $$ LANGUAGE sql;').join(), /^unprovable/);
  assert.match(flagged("SELECT run_ddl('DROP TABLE runbook.t');").join(), /^contract: a call to run_ddl/);
  assert.match(flagged('WITH d AS (DELETE FROM runbook.t RETURNING 1) SELECT count(*) FROM d;').join(), /^unprovable/);
});

test('CREATE TABLE IF NOT EXISTS does not make a table new', () => {
  assert.match(
    flagged('CREATE TABLE IF NOT EXISTS runbook.run (id TEXT);\nALTER TABLE runbook.run DROP COLUMN note;').join(),
    /ALTER .*DROP/,
  );
});

test('every name in a DROP list is checked', () => {
  assert.match(flagged('CREATE TABLE new_one (x INT);\nDROP TABLE new_one, existing;').join(), /DROP TABLE/);
  assert.deepEqual(flagged('CREATE TABLE a1 (x INT);\nCREATE TABLE a2 (x INT);\nDROP TABLE IF EXISTS a1, a2 CASCADE;'), []);
});

test('every ALTER kind is covered', () => {
  for (const sql of [
    'ALTER SCHEMA runbook RENAME TO rb;',
    "ALTER TYPE runbook.status RENAME VALUE 'a' TO 'b';",
    'ALTER SEQUENCE runbook.s RESTART;',
    'ALTER VIEW runbook.v RENAME TO w;',
    'ALTER INDEX runbook.i RENAME TO j;',
    'ALTER TABLE runbook.t ALTER COLUMN c SET DEFAULT 1;',
    'ALTER TABLE runbook.t ENABLE ROW LEVEL SECURITY;',
  ]) {
    assert.match(flagged(sql).join(), /^contract/, sql);
  }
  // A new enum value is checkable only by people (read-tolerant first).
  assert.match(flagged("ALTER TYPE runbook.status ADD VALUE 'new';").join(), /^unprovable: a new value/);
});

test('DELETE and UPDATE need an annotation; UPDATE may be an expand-only backfill', () => {
  assert.match(flagged('DELETE FROM runbook.t;').join(), /^contract: DELETE/);
  assert.match(flagged('DELETE FROM runbook.t WHERE x = 1;').join(), /^contract: DELETE/);
  assert.match(flagged('UPDATE runbook.t SET a = 1;').join(), /^unprovable: UPDATE/);
  assert.match(flagged('TRUNCATE runbook.t;').join(), /^contract: TRUNCATE/);
});

test('inline constraints on ADD COLUMN are constraints on an existing table', () => {
  for (const sql of [
    'ALTER TABLE runbook.t ADD COLUMN c INT CHECK (c > 0);',
    'ALTER TABLE runbook.t ADD COLUMN c TEXT UNIQUE;',
    'ALTER TABLE runbook.t ADD COLUMN c TEXT REFERENCES runbook.u (id);',
    'ALTER TABLE runbook.t ADD COLUMN c TEXT PRIMARY KEY;',
    'ALTER TABLE runbook.t ADD COLUMN c TEXT CONSTRAINT c_ck CHECK (c <> \'\');',
  ]) {
    assert.match(flagged(sql).join(), /^contract: .*constraint/, sql);
  }
  assert.deepEqual(flagged('ALTER TABLE runbook.t ADD COLUMN c TEXT DEFAULT \'unique\';'), []);
});

test('triggers: one statement each; one that deletes or updates rows of an existing table needs review', () => {
  const sql = 'CREATE TRIGGER t_ai AFTER INSERT ON runbook.t BEGIN\n  INSERT INTO u VALUES (1);\n  DELETE FROM v WHERE x = new.x;\nEND;';
  // SQLite trigger bodies (Postgres has none: UPD1-31).
  const lite = (q) => findings(q, 'sqlite').map((f) => `${f.kind}: ${f.rule}`);
  assert.equal(statements(sql, 'sqlite').length, 1);
  assert.match(lite(sql).join(), /^unprovable: a trigger on an existing table/);
  assert.deepEqual(lite('CREATE TABLE n (x INT);\nCREATE TRIGGER n_ai AFTER INSERT ON n BEGIN DELETE FROM n WHERE x < 0; END;'), []);
  // Any trigger on an existing table needs review (re-check #3).
  assert.match(lite('CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO u VALUES (new.x); END;').join(), /^unprovable: a trigger on an existing table/);
  assert.deepEqual(flagged('CREATE TABLE f (x INT REFERENCES g (id) ON DELETE CASCADE ON UPDATE CASCADE);'), []);
});

test('an unknown statement is not assumed safe; OR REPLACE replaces', () => {
  assert.match(flagged('MERGE INTO runbook.t USING s ON true WHEN MATCHED THEN DELETE;').join(), /^unprovable/);
  assert.match(flagged('CREATE OR REPLACE VIEW runbook.v AS SELECT 1;').join(), /^contract: CREATE OR REPLACE/);
  assert.match(flagged('REVOKE SELECT ON runbook.t FROM PUBLIC;').join(), /^contract: REVOKE/);
});

test('annotations: expand-only only for what cannot be checked; unused-since must name a release that exists', () => {
  const pg = 'crates/runbook-store-postgres/migrations';
  const run = (sql, releases = []) => {
    const root = fixture({ [`${pg}/0031_x.sql`]: sql });
    try {
      return check({ root, version: '0.4.0', baseline: {}, releases });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  };
  assert.deepEqual(run("-- expand-only: backfill the new column\nUPDATE runbook.t SET n = '' WHERE n IS NULL;"), []);
  assert.match(run('-- expand-only: please\nDROP TABLE runbook.t;').join(), /expand-only does not cover/);
  assert.deepEqual(run('-- contract(unused-since 0.2.0): 0.2.0 stopped using it\nDROP TABLE runbook.t;', ['0.1.0', '0.2.0']), []);
  assert.match(
    run('-- contract(unused-since 0.2.5): no such release\nDROP TABLE runbook.t;', ['0.1.0', '0.2.0']).join(),
    /no release 0\.2\.5/,
  );
});

test('the rollback floor is the newest unused-since release, or the workspace metadata', () => {
  const root = fixture({
    'crates/runbook-store-postgres/migrations/0031_a.sql': '-- contract(unused-since 0.2.0): a\nDROP TABLE runbook.a;\n',
    'crates/runbook-store-postgres/migrations/0032_b.sql': '-- contract(unused-since 0.3.1): b\nDROP TABLE runbook.b;\n',
    'cloudflare/migrations/d1/1015_c.sql': '-- contract(unused-since 0.2.2): c\nDROP TABLE c;\n',
  });
  try {
    writeFileSync(join(root, 'Cargo.toml'), '[workspace.package]\nversion = "0.4.0"\n');
    assert.equal(rollbackFloor(root, 'postgres'), '0.3.1');
    assert.equal(rollbackFloor(root, 'd1'), '0.2.2');
    assert.equal(rollbackFloor(root, 'execution_do'), null);
    writeFileSync(join(root, 'Cargo.toml'), '[workspace.package]\nversion = "0.4.0"\n\n[workspace.metadata.runbook]\nrollback-floor = "0.3.5"\n');
    assert.equal(rollbackFloor(root, 'postgres'), '0.3.5');
    assert.equal(rollbackFloor(root, 'd1'), '0.3.5');
    assert.equal(rollbackFloor(root, 'webhook_do'), '0.3.5');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  assert.equal(rollbackFloor(repo, 'postgres'), null, 'no contract migration yet');
});

// --- re-check (UPD1 re-check #3, #5) ---------------------------------------------

test('a trigger on a table not created in the file cannot be checked, in either dialect', () => {
  const pg = findings('CREATE TRIGGER t_ai AFTER INSERT ON runbook.run FOR EACH ROW EXECUTE FUNCTION runbook.f();', 'postgres');
  assert.match(pg.map((f) => `${f.kind}: ${f.rule}`).join(), /^unprovable: a trigger on an existing table/);
  const lite = findings('CREATE TRIGGER t_ai AFTER INSERT ON run BEGIN INSERT INTO log VALUES (new.id); END;', 'sqlite');
  assert.match(lite.map((f) => `${f.kind}: ${f.rule}`).join(), /^unprovable: a trigger on an existing table/);
  assert.deepEqual(findings('CREATE TABLE n (x INT);\nCREATE TRIGGER n_ai AFTER INSERT ON n BEGIN INSERT INTO log VALUES (new.x); END;', 'sqlite'), []);
});

test('names are qualified, and TEMP tables are never new', () => {
  for (const sql of [
    'CREATE TABLE scratch.run (id TEXT);\nALTER TABLE runbook.run DROP COLUMN note;',
    'CREATE TABLE scratch.run (id TEXT);\nDROP TABLE runbook.run;',
    'CREATE TEMP TABLE run (id TEXT);\nDROP TABLE run;',
    'CREATE TEMPORARY TABLE run (id TEXT);\nALTER TABLE run RENAME TO r2;',
  ]) {
    assert.match(findings(sql, 'postgres').map((f) => f.kind).join(), /contract/, sql);
  }
  assert.deepEqual(findings('CREATE TABLE runbook.n (x INT);\nALTER TABLE runbook.n DROP COLUMN x;', 'postgres'), []);
});

test('SQLite comments do not nest; Postgres ones do', () => {
  const sql = '/* /* */ DROP TABLE x; -- */';
  assert.match(findings(sql, 'sqlite').map((f) => f.rule).join(), /DROP TABLE/);
  assert.deepEqual(findings(sql, 'postgres'), [], 'in Postgres the whole line is one comment');
});

test('INSERT OR REPLACE, REPLACE INTO and upserts change existing rows', () => {
  assert.match(findings('INSERT OR REPLACE INTO run (id) VALUES (1);', 'sqlite').map((f) => `${f.kind}: ${f.rule}`).join(), /^contract: .*replaces existing rows/);
  assert.match(findings('REPLACE INTO run (id) VALUES (1);', 'sqlite').map((f) => f.kind).join(), /contract/);
  assert.match(findings('INSERT INTO run (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET n = 2;', 'postgres').map((f) => f.kind).join(), /unprovable/);
  assert.deepEqual(findings("INSERT INTO run (id, at) VALUES ('a', now());", 'postgres'), [], 'a plain function is fine');
});

test('a statement calling a function that is not known to be plain is a contract change', () => {
  for (const sql of ["SELECT query_to_xml('DR'||'OP TABLE runbook.t', true, false, '');", "SELECT runbook.cleanup();", "INSERT INTO n SELECT dblink_exec('x');"]) {
    assert.match(findings(sql, 'postgres').map((f) => `${f.kind}: ${f.rule}`).join(), /^contract: a call to/, sql);
  }
  assert.deepEqual(findings("SELECT setval('runbook.s', 1);", 'postgres').length, 1);
  assert.deepEqual(findings('SELECT lower(x), coalesce(y, 0) FROM runbook.t;', 'postgres'), []);
});

test('dynamic SQL is a contract change; UPDATE ... = NULL is too', () => {
  assert.match(findings("DO $$ BEGIN EXECUTE 'CREATE INDEX i ON t (x)'; END $$;", 'postgres').map((f) => f.kind).join(), /^contract$/);
  assert.match(findings('UPDATE runbook.t SET note = NULL;', 'postgres').map((f) => `${f.kind}: ${f.rule}`).join(), /^contract: .*NULL/);
  assert.match(findings("UPDATE runbook.t SET note = '' WHERE note IS NULL;", 'postgres').map((f) => f.kind).join(), /^unprovable$/);
});

test('expand-only never covers a statement that names DROP, DELETE, TRUNCATE, ALTER or RENAME', () => {
  const pg = 'crates/runbook-store-postgres/migrations';
  const run = (sql) => {
    const root = fixture({ [`${pg}/0031_x.sql`]: sql });
    try {
      return check({ root, version: '0.4.0', baseline: {}, releases: [] });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  };
  for (const sql of [
    "-- expand-only: just a backfill\nWITH d AS (DELETE FROM runbook.t RETURNING 1) SELECT count(*) FROM d;",
    "-- expand-only: a function\nCREATE FUNCTION runbook.f() RETURNS void AS $$ DROP TABLE runbook.t; $$ LANGUAGE sql;",
    "-- expand-only: trigger\nCREATE TRIGGER t_au AFTER UPDATE ON runbook.t FOR EACH ROW EXECUTE FUNCTION runbook.g('ALTER');",
  ]) {
    assert.match(run(sql).join(), /expand-only cannot cover/, sql);
  }
  assert.deepEqual(run("-- expand-only: fill the new column\nUPDATE runbook.t SET n = '' WHERE n IS NULL;"), []);
});

test('one annotation grammar: every near miss is rejected, and all parsers agree on the floor', () => {
  const sql = readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'fixtures', 'floor-annotations.sql'), 'utf8');
  const expected = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'fixtures', 'floor-annotations.json'), 'utf8'));
  assert.equal(annotationFloor([sql]), expected.floor);
  const root = fixture({ 'crates/runbook-store-postgres/migrations/0031_x.sql': sql });
  try {
    const errors = check({ root, version: '0.4.0', baseline: {}, releases: [] });
    const malformed = errors.filter((e) => /malformed annotation/.test(e)).map((e) => Number(/0031_x\.sql:(\d+)/.exec(e)[1]));
    assert.deepEqual(malformed, expected.malformed_lines);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('the sign-in service\'s migrations are checked and count toward the floor', () => {
  assert.ok(DIRECTORIES.includes('auth/drizzle/pg') && DIRECTORIES.includes('auth/drizzle/sqlite'));
  const root = fixture({
    'auth/drizzle/sqlite/0001_x.sql': '-- contract(unused-since 0.2.0): x\nDROP TABLE `auth_account`;\n',
    'auth/drizzle/pg/0001_x.sql': 'ALTER TABLE "auth"."user" DROP COLUMN "name";\n',
  });
  try {
    writeFileSync(join(root, 'Cargo.toml'), '[workspace.package]\nversion = "0.4.0"\n');
    const errors = check({ root, version: '0.4.0', baseline: {}, releases: [] });
    assert.equal(errors.length, 1, errors.join('\n'));
    assert.match(errors[0], /auth\/drizzle\/pg\/0001_x\.sql/);
    assert.equal(rollbackFloor(root, 'd1'), '0.2.0');
    assert.equal(rollbackFloor(root, 'postgres'), null);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// --- follow-up (UPD1-31: the check guards against honest mistakes and cheap
// tricks; exotic lexer tricks are best-effort) --------------------------------

const kinds = (sql, dialect = 'postgres', history = '') => findings(sql, dialect, { history }).map((f) => `${f.kind}: ${f.rule}`);

test('every trigger body is read, on a table this file created too', () => {
  const lite = (body) => `CREATE TABLE n (x INT);\nCREATE TRIGGER n_ai AFTER INSERT ON n BEGIN ${body} END;`;
  assert.match(kinds(lite('DELETE FROM run;'), 'sqlite').join(), /^unprovable: a trigger writes run/);
  assert.match(kinds(lite('UPDATE run SET x = 1;'), 'sqlite').join(), /^unprovable: a trigger writes run/);
  assert.match(kinds(lite('INSERT OR REPLACE INTO run VALUES (1);'), 'sqlite').join(), /^unprovable: a trigger writes run/);
  assert.match(kinds(lite("SELECT purge_all();"), 'sqlite').join(), /^contract: a call to purge_all/);
  assert.deepEqual(kinds(lite('INSERT INTO n_log VALUES (new.x); DELETE FROM n WHERE x < 0;'), 'sqlite'), []);
  const pg = 'CREATE TABLE runbook.n (x INT);\nCREATE TRIGGER n_ai AFTER INSERT ON runbook.n FOR EACH ROW EXECUTE FUNCTION runbook.purge();';
  assert.match(kinds(pg).join(), /^unprovable: a trigger runs runbook\.purge/);
});

test('a dropped index or trigger is only replaced by the same kind on the same table, with the same uniqueness and columns', () => {
  const history = 'CREATE UNIQUE INDEX run_key ON runbook.run (org_id, key);\nCREATE TRIGGER run_guard BEFORE UPDATE ON runbook.run FOR EACH ROW EXECUTE FUNCTION runbook.guard();';
  // Same definition: a replace.
  assert.deepEqual(kinds('DROP INDEX runbook.run_key;\nCREATE UNIQUE INDEX run_key ON runbook.run (org_id, key);', 'postgres', history), []);
  for (const sql of [
    'DROP INDEX runbook.run_key;\nCREATE INDEX run_key ON runbook.run (org_id, key);', // uniqueness lost
    'DROP INDEX runbook.run_key;\nCREATE UNIQUE INDEX run_key ON runbook.run (key);', // columns changed
    'DROP INDEX runbook.run_key;\nCREATE UNIQUE INDEX run_key ON runbook.other (org_id, key);', // another table
    'DROP TRIGGER run_guard ON runbook.run;\nCREATE TRIGGER run_guard BEFORE UPDATE ON runbook.other FOR EACH ROW EXECUTE FUNCTION runbook.guard();',
    'DROP INDEX runbook.unknown;\nCREATE INDEX unknown ON runbook.run (x);', // no original to compare with
  ]) {
    assert.match(kinds(sql, 'postgres', history).join(), /contract: DROP (INDEX|TRIGGER)/, sql);
  }
  const liteHistory = 'CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO u VALUES (1); END;';
  assert.match(
    kinds('DROP TRIGGER t_ai;\nCREATE TRIGGER t_ai AFTER INSERT ON t2 BEGIN INSERT INTO u VALUES (1); END;', 'sqlite', liteHistory).join(),
    /contract: DROP TRIGGER/,
  );
});

test('names keep their quoting and case, and end at a real terminator', () => {
  assert.match(kinds('CREATE TABLE "runbook.run" (x INT);\nDROP TABLE runbook.run;').join(), /contract: DROP TABLE/);
  assert.match(kinds('CREATE TABLE "Run" (x INT);\nDROP TABLE run;').join(), /contract: DROP TABLE/);
  assert.deepEqual(kinds('CREATE TABLE "run" (x INT);\nDROP TABLE run;'), [], '"run" folds to run');
  assert.match(kinds('CREATE TABLE runé (x INT);\nDROP TABLE run;').join(), /contract: DROP TABLE/, 'runé is not run');
});

test('SET search_path cannot be checked; set_config is a call', () => {
  assert.match(kinds('SET search_path TO scratch, runbook;').join(), /^unprovable: SET search_path/);
  assert.match(kinds("SELECT set_config('search_path', 'scratch', false);").join(), /^contract: a call to set_config/);
});

test('only DROP TABLE of a table this file created is exempt; the object kind matters', () => {
  assert.match(kinds('CREATE TABLE runbook (x INT);\nDROP SCHEMA runbook CASCADE;').join(), /contract: DROP SCHEMA/);
  assert.match(kinds('CREATE TABLE v (x INT);\nDROP VIEW v;').join(), /contract: DROP VIEW/);
  assert.deepEqual(kinds('CREATE TABLE n (x INT);\nDROP TABLE n;'), []);
});

test('quoted calls, and calls inside CREATE TABLE ... AS and materialized views, go through the call check', () => {
  assert.match(kinds('SELECT "purge_all"();').join(), /^contract: a call to "purge_all"/);
  assert.match(kinds('CREATE TABLE runbook.n AS (SELECT runbook.purge() AS x);').join(), /^contract: a call to runbook\.purge/);
  assert.match(kinds('CREATE TABLE runbook.n AS VALUES (runbook.purge());').join(), /^contract: a call to runbook\.purge/);
  assert.match(kinds('CREATE MATERIALIZED VIEW runbook.m AS SELECT runbook.purge();').join(), /^contract: a call to runbook\.purge/);
  assert.deepEqual(kinds('CREATE TABLE runbook.n AS SELECT lower(x) AS x FROM runbook.t;'), []);
});

test('COPY ... FROM PROGRAM and CREATE RULE are contract changes, never expand-only', () => {
  assert.match(kinds("COPY runbook.t FROM PROGRAM 'curl x';").join(), /^contract: COPY \.\.\. PROGRAM/);
  assert.match(kinds('CREATE RULE r AS ON INSERT TO runbook.t DO INSTEAD NOTHING;').join(), /^contract: CREATE RULE/);
});

test('Postgres has no BEGIN ... END trigger bodies: a later statement is its own', () => {
  const sql = 'CREATE TABLE runbook.n (x INT);\nCREATE TRIGGER n_ai AFTER INSERT ON runbook.n FOR EACH ROW EXECUTE FUNCTION begin_log();\nDROP TABLE runbook.t;';
  assert.equal(statements(sql, 'postgres').length, 3);
  assert.match(kinds(sql).join(), /contract: DROP TABLE/);
});

test('best-effort: SQLite [bracket] names and non-ASCII dollar tags', () => {
  assert.match(kinds('CREATE TABLE [run] (x INT);\nDROP TABLE [other];', 'sqlite').join(), /contract: DROP TABLE/);
  assert.deepEqual(kinds('CREATE TABLE [n] (x INT);\nDROP TABLE [n];', 'sqlite'), []);
  assert.equal(statements('DO $é$ BEGIN PERFORM 1; END $é$;\nDROP TABLE runbook.t;', 'postgres').length, 2);
});

test('check() compares a replace with the definition in an earlier file of the same directory', () => {
  const pg = 'crates/runbook-store-postgres/migrations';
  const root = fixture({
    [`${pg}/0030_old.sql`]: 'CREATE UNIQUE INDEX run_key ON runbook.run (key);',
    [`${pg}/0032_x.sql`]: 'DROP INDEX runbook.run_key;\nCREATE INDEX run_key ON runbook.run (key);',
  });
  try {
    const errors = check({ root, version: '0.4.0', baseline: { [pg]: ['0030_old.sql'] }, releases: [] });
    assert.match(errors.join(), /0032_x\.sql:1: DROP INDEX/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
