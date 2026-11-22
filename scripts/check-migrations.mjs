#!/usr/bin/env node
// UPD1: the expand/contract rule for schema migrations.
//
// Rollback is "redeploy the previous release's code", with the schema left
// as the new release migrated it (deploy/update.sh, cloudflare/scripts/
// update.mjs). That is safe only if every migration keeps the previous
// release's code working. The check is an allowlist: a new migration may
// create tables, indexes, views, schemas, sequences and types, add columns
// (nullable, or NOT NULL with a DEFAULT, without inline constraints) and
// insert rows. Everything else is flagged, in one of two kinds:
//
// - contract: it can break code written for the old shape (any DROP,
//   RENAME, other ALTER, a new constraint or unique index on an existing
//   table, OR REPLACE, REVOKE, DELETE, TRUNCATE). It needs, on the comment
//   line(s) just above it, the release that stopped using the old shape:
//
//   -- contract(unused-since 0.3.0): 0.3.0 stopped reading run.legacy_note
//   ALTER TABLE runbook.run DROP COLUMN legacy_note;
//
// The named release must be older than this one (the workspace version in
// Cargo.toml) and, once releases are tagged, exist: the code that stops
// using a shape and the migration that removes it ship in different
// releases (the two-release contract). The newest such release is the
// rollback floor: older code is not safe on the schema (rollbackFloor).
//
// - unprovable: the check cannot tell (DO blocks, EXECUTE, functions,
//   UPDATE, a new enum value, a trigger that deletes or updates rows of an
//   existing table, a statement it does not know, DDL inside a string). It
//   needs a contract annotation or `-- expand-only: <why the previous
//   release keeps working>`, which a reviewer then checks.
//
// Comments (line and nested block) are removed and statements split on
// every `;` outside quotes, dollar quoting and SQLite trigger bodies, so
// `/* x */ DROP ...` or `SELECT 1; DROP ...` on one line are seen.
//
// Covers the Postgres migrations, the D1 migrations and the Durable Object
// schemas (ExecutionDO, OrgDO, WebhookDO). Files that existed when the rule
// was introduced are listed in BASELINE and not checked (several are
// contract changes made before any release); never add a file to it.
//
//   node scripts/check-migrations.mjs        (scripts/check.sh runs it)
//   node --test scripts/tests/               (its tests)
import { spawnSync } from 'node:child_process';
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

export const DIRECTORIES = [
  'crates/runbook-store-postgres/migrations',
  'cloudflare/migrations/d1',
  'cloudflare/migrations/do',
  'cloudflare/migrations/orgdo',
  'cloudflare/migrations/webhookdo',
  // The sign-in service's tables: Postgres (native) and D1 (Workers, applied
  // by cloudflare/scripts/update.mjs).
  'auth/drizzle/pg',
  'auth/drizzle/sqlite',
];

/** Postgres directories use Postgres's lexing (nested comments, dollar quoting); the rest SQLite's. */
export const dialectOf = (dir) => (/postgres|drizzle\/pg/.test(dir) ? 'postgres' : 'sqlite');

/** The directories each store's migrations live in (rollback floors are per store). */
export const STORE_DIRS = {
  postgres: ['crates/runbook-store-postgres/migrations', 'auth/drizzle/pg'],
  d1: ['cloudflare/migrations/d1', 'auth/drizzle/sqlite'],
  execution_do: ['cloudflare/migrations/do'],
  org_do: ['cloudflare/migrations/orgdo'],
  webhook_do: ['cloudflare/migrations/webhookdo'],
};

/** Files present when UPD1 introduced the rule (pinned by name). */
export const BASELINE = {
  'crates/runbook-store-postgres/migrations': [
    '0001_history.sql', '0002_execution_catalog.sql', '0003_directory.sql',
    '0004_directory_member_org.sql', '0005_work_lease.sql', '0006_org_roles.sql',
    '0007_blobs_tombstones.sql', '0008_credentials.sql', '0009_catalog_publication.sql',
    '0010_skills.sql', '0011_instance_bootstrap.sql', '0012_run_search.sql',
    '0013_directory_api.sql', '0014_start_admission.sql', '0015_draft_source_bytes.sql',
    '0016_owner_recovery_audit.sql', '0017_claims.sql', '0018_start_replay_retention.sql',
    '0019_execution_credentials.sql', '0020_actor_replay_step_index.sql', '0021_webhooks.sql',
    '0022_webhook_follow_ups.sql', '0023_review_indexes.sql', '0024_step_run_tombstone.sql',
    '0025_inbox_index.sql', '0026_memory.sql', '0027_auth_links_invitations_device.sql',
    '0028_uploads.sql', '0029_archives_retention.sql', '0030_quotas_usage.sql',
  ],
  'cloudflare/migrations/d1': [
    '0001_history.sql', '0002_execution_catalog.sql', '0003_directory.sql',
    '0004_directory_member_org.sql', '0005_work_lease.sql', '0006_org_roles.sql',
    '0007_blobs_tombstones.sql', '0008_credentials.sql', '0009_catalog_publication.sql',
    '0010_skills.sql', '0011_instance_bootstrap.sql', '0012_run_search.sql',
    '0013_directory_api.sql', '0014_start_admission.sql', '0015_draft_source_bytes.sql',
    '0016_owner_recovery_audit.sql', '0017_claims.sql', '0018_start_replay_retention.sql',
    '0019_execution_credentials.sql', '0020_actor_replay_step_index.sql', '0021_webhooks.sql',
    '0022_webhook_follow_ups.sql', '0023_review_indexes.sql', '0024_step_run_tombstone.sql',
    '0025_inbox_index.sql', '0026_memory.sql', '0027_auth_links_invitations_device.sql',
    '0028_uploads.sql', '0029_archives_retention.sql',
    '1001_d1_guard.sql', '1002_execution_token_route.sql', '1003_memory_fts_words.sql',
    '1004_run_search_words_and_admission_notice.sql', '1005_admission_pool_revision.sql',
    '1006_object_purge.sql', '1007_archive_version.sql', '1008_supersede_notice.sql',
    '1009_webhook_log_to_objects.sql', '1010_runbook_source_digests.sql', '1011_run_release.sql',
    '1012_cron_backoff.sql', '1013_webhook_subscription_deleted.sql', '1014_run_release_backoff.sql',
  ],
  'cloudflare/migrations/do': [
    '0001_execution.sql', '0002_session_fence.sql', '0003_log_chunk.sql',
    '0004_import_hold.sql', '0005_import_hold_id.sql',
  ],
  'cloudflare/migrations/orgdo': ['0001_quotas.sql'],
  'cloudflare/migrations/webhookdo': ['0001_delivery_log.sql', '0002_lifecycle.sql'],
  'auth/drizzle/pg': ['0000_init.sql'],
  'auth/drizzle/sqlite': ['0000_init.sql'],
};

// UPD1 re-check #5: the one annotation grammar, at the start of a line,
// exactly so. Every floor parser (here, cloudflare/scripts/update.mjs,
// crates/runbook-server/src/rollback_floor.rs) reads only this; the check
// rejects anything that looks like an annotation but is not exactly it.
export const CONTRACT = /^-- contract\(unused-since (\d+\.\d+\.\d+)\): (\S.*)$/;
export const EXPAND_ONLY = /^-- expand-only: (\S.*)$/;
const NEAR_ANNOTATION = /contract\s*\(|unused[-_ ]?since|expand[-_ ]?only/i;

/** The floor a set of migration texts declares: the newest exact
 * `contract(unused-since X.Y.Z)`, or null. */
export function annotationFloor(texts) {
  let floor = null;
  for (const text of texts) {
    for (const line of text.split('\n')) {
      const m = CONTRACT.exec(line);
      if (m && semver(m[1]) && (!floor || older(semver(floor), semver(m[1])))) floor = m[1];
    }
  }
  return floor;
}

/** `[major, minor, patch]`, or null. */
export function semver(text) {
  const m = /^(\d+)\.(\d+)\.(\d+)$/.exec(text ?? '');
  return m ? m.slice(1, 4).map(Number) : null;
}
export function older(a, b) {
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] < b[i];
  return false;
}

/** The workspace release version from Cargo.toml. */
export function workspaceVersion(root) {
  const toml = readFileSync(join(root, 'Cargo.toml'), 'utf8');
  const section = toml.slice(toml.indexOf('[workspace.package]'));
  const m = /^version\s*=\s*"([^"]+)"/m.exec(section);
  if (!m) throw new Error('no [workspace.package] version in Cargo.toml');
  return m[1];
}

/**
 * Split a file into statements. Each is `{ text, scan, line, comments }`:
 * `text` without comments, `scan` the same with string literals and
 * dollar-quoted bodies emptied (for keyword matching), the line it starts
 * on, and the `--` comment lines directly above it (a blank line or code
 * in between detaches them). `;` ends a statement outside quotes, dollar
 * quoting and the BEGIN ... END body of a CREATE TRIGGER.
 */
export function statements(sql, dialect = 'postgres') {
  const postgres = dialect === 'postgres';
  const out = [];
  let text = '';
  let scan = '';
  let start = 0;
  let line = 1;
  let lineStart = 0;
  let pending = [];
  let lineHasContent = false;
  const push = () => {
    if (text.trim()) out.push({ text: text.trim(), scan: scan.trim(), line: start, comments: pending.length ? pending : [] });
    if (text.trim()) pending = [];
    text = '';
    scan = '';
  };
  const add = (raw, scanned = raw) => {
    if (!text.trim() && raw.trim()) start = line;
    text += raw;
    scan += scanned;
  };
  // SQLite's trigger bodies are BEGIN ... END with `;` inside; Postgres has
  // none (its triggers call functions), so only SQLite gets this mode.
  const isTrigger = () => !postgres && /^\s*CREATE\s+(?:TEMP(?:ORARY)?\s+)?TRIGGER\b/i.test(scan) && /\bBEGIN\b/i.test(scan);
  const quoted = (i, close) => {
    let j = i + 1;
    for (; j < sql.length; j++) {
      if (sql[j] === close && sql[j + 1] === close) j++;
      else if (sql[j] === close) break;
    }
    return j;
  };
  let i = 0;
  while (i < sql.length) {
    const c = sql[i];
    const next = sql[i + 1];
    if (c === '\n') {
      if (!lineHasContent && !text.trim()) pending = [];
      lineHasContent = false;
      line++;
      lineStart = i + 1;
      add('\n');
      i++;
      continue;
    }
    if (c === '-' && next === '-') {
      const end = sql.indexOf('\n', i);
      // The whole line as written (indentation included), so the annotation
      // grammar can insist on column 0.
      const whole = sql.slice(lineStart, end < 0 ? sql.length : end);
      if (!text.trim() && !sql.slice(lineStart, i).trim()) pending.push(whole);
      lineHasContent = true;
      i = end < 0 ? sql.length : end;
      continue;
    }
    if (c === '/' && next === '*') {
      // Postgres nests block comments; SQLite ends at the first */.
      let depth = 1;
      let j = i + 2;
      while (j < sql.length && depth > 0) {
        if (postgres && sql[j] === '/' && sql[j + 1] === '*') (depth++, (j += 2));
        else if (sql[j] === '*' && sql[j + 1] === '/') (depth--, (j += 2));
        else {
          if (sql[j] === '\n') line++;
          j++;
        }
      }
      add(' ');
      lineHasContent = true;
      i = j;
      continue;
    }
    lineHasContent = lineHasContent || !/\s/.test(c);
    if (c === "'") {
      const backslash = postgres && /[eE]$/.test(text) && !/[\w]$/.test(text.slice(0, -1));
      let j = i + 1;
      for (; j < sql.length; j++) {
        if (backslash && sql[j] === '\\') j++;
        else if (sql[j] === "'" && sql[j + 1] === "'") j++;
        else if (sql[j] === "'") break;
      }
      const literal = sql.slice(i, j + 1);
      line += (literal.match(/\n/g) ?? []).length;
      add(literal, "''");
      i = j + 1;
      continue;
    }
    if (c === '"' || (!postgres && c === '`')) {
      const j = quoted(i, c);
      const inner = sql.slice(i + 1, j).replaceAll(c + c, c);
      add(`"${inner.replaceAll('"', '""')}"`);
      i = j + 1;
      continue;
    }
    if (!postgres && c === '[') {
      // SQLite's [bracketed] names, written as "quoted" ones.
      const j = sql.indexOf(']', i + 1);
      const end = j < 0 ? sql.length : j;
      add(`"${sql.slice(i + 1, end).replaceAll('"', '""')}"`);
      i = end + 1;
      continue;
    }
    const dollar = postgres && c === '$' ? /^\$([\p{L}_][\p{L}\p{N}_]*)?\$/u.exec(sql.slice(i)) : null;
    if (dollar && !/[\w$]$/.test(text)) {
      const close = sql.indexOf(dollar[0], i + dollar[0].length);
      const end = close < 0 ? sql.length : close + dollar[0].length;
      const body = sql.slice(i, end);
      line += (body.match(/\n/g) ?? []).length;
      add(body, `${dollar[0]}${dollar[0]}`);
      i = end;
      continue;
    }
    if (c === ';') {
      if (isTrigger() && !/\bEND\s*$/i.test(scan)) {
        add(';');
        i++;
        continue;
      }
      push();
      i++;
      continue;
    }
    add(c);
    i++;
  }
  push();
  return out;
}

// A name: dot-separated parts, each "quoted" or a run of letters (any
// script), digits, `_` and `$`, ending where such characters end
// (UPD1-31: `runé` is not `run`). Regexes using it need the `u` flag.
const PART = String.raw`(?:"(?:[^"]|"")+"|[\p{L}_][\p{L}\p{N}_$]*)`;
const NAME = String.raw`${PART}(?:\.${PART})*(?![\p{L}\p{N}_$])`;
/** A name as written, normalized but still qualified (`runbook.run` and
 * `scratch.run` differ); SQLite's `main.` is its default schema. */
/** A name as the database resolves it: unquoted parts fold to lower case; a
 * quoted part keeps its case (and its dots: `"runbook.run"` is one part) and
 * equals the unquoted spelling only when that is what it holds. SQLite's
 * `main.` is its default schema. */
function qualified(name) {
  const parts = [...name.trim().matchAll(new RegExp(PART, 'gu'))].map(([p]) => {
    if (!p.startsWith('"')) return p.toLowerCase();
    const inner = p.slice(1, -1).replaceAll('""', '"');
    return /^[a-z_][a-z0-9_$]*$/.test(inner) ? inner : `"${inner.replaceAll('"', '""')}"`;
  });
  return parts.join('.').replace(/^main\./, '');
}
const lastPart = (name) => qualified(name).split(/\.(?=(?:[^"]*"[^"]*")*[^"]*$)/).pop();
/** Split on commas outside parentheses. */
function topLevel(list) {
  const parts = [];
  let depth = 0;
  let cur = '';
  for (const ch of list) {
    if (ch === '(') depth++;
    if (ch === ')') depth--;
    if (ch === ',' && depth === 0) {
      parts.push(cur.trim());
      cur = '';
    } else cur += ch;
  }
  if (cur.trim()) parts.push(cur.trim());
  return parts;
}

const SAFE_ADD_COLUMN = new RegExp(String.raw`^ADD\s+(?:COLUMN\s+)?(?:IF\s+NOT\s+EXISTS\s+)?(?!(?:CONSTRAINT|CHECK|UNIQUE|PRIMARY|FOREIGN|EXCLUDE)\b)${NAME}\s+(.*)$`, 'isu');

/** Why one ALTER TABLE action on an existing table is not additive, or null. */
function alterTableAction(action) {
  const add = SAFE_ADD_COLUMN.exec(action);
  if (add) {
    const rest = add[1];
    if (/\b(?:CHECK|UNIQUE|PRIMARY\s+KEY|REFERENCES|CONSTRAINT|EXCLUDE)\b/i.test(rest)) {
      return 'ADD COLUMN with an inline constraint adds a constraint on an existing table, which can refuse the previous release\'s writes';
    }
    if (/\bNOT\s+NULL\b/i.test(rest) && !/\b(?:DEFAULT|GENERATED)\b/i.test(rest)) {
      return 'ADD COLUMN ... NOT NULL without a DEFAULT refuses the previous release\'s writes';
    }
    return null;
  }
  if (/\bRENAME\b/i.test(action)) return 'RENAME breaks the previous release\'s queries';
  if (/^DROP\b/i.test(action)) return 'ALTER ... DROP removes what the previous release may still use';
  if (/\bTYPE\b/i.test(action) && /^ALTER\b/i.test(action)) return 'a column type change can break the previous release\'s reads and writes';
  if (/\bSET\s+NOT\s+NULL\b/i.test(action)) return 'SET NOT NULL refuses the previous release\'s writes';
  if (/^ADD\b/i.test(action)) return 'a new constraint on an existing table can refuse the previous release\'s writes';
  return `ALTER TABLE ... ${action.split(/\s+/).slice(0, 3).join(' ')} changes an existing table the previous release uses`;
}

const SAFE_KINDS = /^(?:INSERT|SELECT|VALUES|GRANT|COMMENT|PRAGMA|SET|ANALYZE|VACUUM|REINDEX|BEGIN|COMMIT|END|SAVEPOINT|RELEASE|LOCK)\b/i;
const HIDDEN_DDL = /\b(?:DROP|TRUNCATE|RENAME|ALTER|EXECUTE|DELETE|UPDATE)\b/i;
// What expand-only can never vouch for (re-check #3): a statement whose text,
// strings and dollar-quoted bodies included, names one of these.
const DESTRUCTIVE_WORDS = /\b(?:DROP|DELETE|TRUNCATE|ALTER|RENAME)\b/i;
// Functions with no effect beyond their value; any other call (query_to_xml,
// dblink_exec, a project function) can run SQL, so it is a contract change.
const PLAIN_FUNCTIONS = new Set(
  ('count sum min max avg total lower upper coalesce ifnull nullif length char_length octet_length substr substring trim ltrim rtrim ' +
    'replace abs round floor ceil ceiling cast now current_timestamp current_date julianday strftime datetime date time unixepoch ' +
    'gen_random_uuid uuid_generate_v4 random hex unhex md5 sha256 encode decode json json_extract json_object json_array json_build_object ' +
    'jsonb_build_object to_json to_jsonb json_agg jsonb_agg array_agg string_agg group_concat lpad rpad concat greatest least extract ' +
    'date_part date_trunc to_timestamp to_char make_interval printf format instr position quote typeof')
    .split(' '),
);
const KEYWORDS_BEFORE_PAREN = /^(?:values|in|exists|as|and|or|not|on|select|from|where|using|over|filter|any|all|returning|into|table|key|references|default|check|unique|primary|foreign|conflict|do|set)$/i;

const SQLITE_TRIGGER_FUNCTIONS = new Set(['randomblob', 'json_array_length']);

/** The first call in `text` to a function not known to be plain, or null. */
function unsafeCall(text, sqliteTrigger = false) {
  for (const m of text.matchAll(new RegExp(String.raw`(${PART}(?:\.${PART})*)\s*\(`, 'gu'))) {
    const raw = m[1];
    const quotedName = raw.includes('"');
    const name = qualified(raw);
    if (!quotedName && KEYWORDS_BEFORE_PAREN.test(name)) continue;
    // A quoted name is a call even when it spells a keyword; a quoted plain
    // function name ("lower") folds to the plain function.
    if (!name.includes('.') && PLAIN_FUNCTIONS.has(name)) continue;
    // Unquoted RAISE is SQLite trigger grammar. The other two are built-ins.
    if (sqliteTrigger && ((!quotedName && name === 'raise') || SQLITE_TRIGGER_FUNCTIONS.has(name))) continue;
    return raw;
  }
  return null;
}

/** The call check for one statement or trigger-body fragment, its INSERT /
 * REPLACE target and column list set aside (they are not calls). */
function unsafeCallIn(text, sqliteTrigger = false) {
  return unsafeCall(text.replace(new RegExp(String.raw`^\s*(?:INSERT\s+(?:OR\s+\w+\s+)?|REPLACE\s+)INTO\s+${NAME}\s*(?:\([^)]*\))?`, 'iu'), ''), sqliteTrigger);
}

/** The text after the first AS outside parentheses (a CREATE TABLE ... AS or
 * materialized view's query), or null. */
function queryAfterAs(text) {
  let depth = 0;
  for (let i = 0; i < text.length; i++) {
    if (text[i] === '(') depth++;
    else if (text[i] === ')') depth--;
    else if (depth === 0 && /^AS\b/i.test(text.slice(i)) && /\s/.test(text[i - 1] ?? ' ')) return text.slice(i + 2);
  }
  return null;
}

/** What an index or trigger is, for comparing a drop-and-recreate with the
 * original: kind, table, uniqueness, and the columns and predicate (index)
 * or timing and event (trigger), normalized. Null when `text` creates
 * neither. */
function definition(text) {
  const t = text.replace(/\s+/g, ' ').trim();
  let m = new RegExp(String.raw`^CREATE\s+(UNIQUE\s+)?INDEX\s+(?:CONCURRENTLY\s+)?(?:IF\s+NOT\s+EXISTS\s+)?(${NAME})\s+ON\s+(?:ONLY\s+)?(${NAME})\s*(.*)$`, 'iu').exec(t);
  if (m) return { kind: 'INDEX', name: lastPart(m[2]), table: qualified(m[3]), unique: Boolean(m[1]), shape: m[4].replace(/\s+/g, '').toLowerCase() };
  m = new RegExp(String.raw`^CREATE\s+(?:TEMP(?:ORARY)?\s+)?TRIGGER\s+(?:IF\s+NOT\s+EXISTS\s+)?(${NAME})\s+(.*?)\s+ON\s+(${NAME})`, 'iu').exec(t);
  if (m) return { kind: 'TRIGGER', name: lastPart(m[1]), table: qualified(m[3]), unique: false, shape: m[2].toLowerCase() };
  return null;
}
const sameDefinition = (a, b) => a && b && a.kind === b.kind && a.table === b.table && a.unique === b.unique && a.shape === b.shape;

/**
 * The expand/contract findings for one file: `{ line, kind, rule,
 * statement, text, comments }` per statement that may break the previous
 * release's code (`contract`) or that the check cannot judge
 * (`unprovable`). Tables created (not TEMP, not `IF NOT EXISTS`) earlier in
 * the same file are new, matched by qualified name, so changing them is
 * free; a DROP INDEX/TRIGGER whose name is created again in the file is a
 * replace. `dialect` is `postgres` or `sqlite` (comment nesting, quoting).
 */
export function findings(sql, dialect = 'postgres', { history = '' } = {}) {
  // What this file creates, by kind and name: only these are new.
  const created = new Set();
  const isNew = (kind, name) => created.has(`${kind}:${qualified(name)}`);
  const out = [];
  const parsed = statements(sql, dialect);
  // The definitions an index or trigger had before (earlier files, then
  // earlier in this file), to compare a drop-and-recreate with.
  const before = new Map();
  // Indexes and triggers dropped here and created again as they were.
  const replacing = new Set();
  for (const s of statements(history, dialect)) {
    const d = definition(s.scan);
    if (d) before.set(`${d.kind}:${d.name}`, d);
  }
  for (const [index, s] of parsed.entries()) {
    const t = s.scan.replace(/\s+/g, ' ').trim();
    const flag = (kind, rule) =>
      out.push({ line: s.line, kind, rule, statement: s.text.replace(/\s+/g, ' ').slice(0, 120), text: s.text, comments: s.comments });
    const contract = (rule) => flag('contract', rule);
    const unprovable = (rule) => flag('unprovable', rule);
    const own = definition(s.scan);
    let m;
    if (/^CREATE\s+OR\s+REPLACE\b/i.test(t)) {
      contract('CREATE OR REPLACE replaces what the previous release uses');
    } else if ((m = new RegExp(String.raw`^CREATE\s+((?:TEMP|TEMPORARY|UNLOGGED)\s+)?(?:VIRTUAL\s+)?TABLE\s+(IF\s+NOT\s+EXISTS\s+)?(${NAME})`, 'iu').exec(t))) {
      // A TEMP table or one that may already exist is never new.
      const temp = /^TEMP/i.test(m[1] ?? '');
      if (!temp && !m[2]) created.add(`TABLE:${qualified(m[3])}`);
      const query = queryAfterAs(t.slice(m[0].length));
      const call = query && unsafeCallIn(query);
      if (call) contract(`a call to ${call} can run any SQL`);
      else if (HIDDEN_DDL.test(t.replace(/\bON\s+(?:DELETE|UPDATE)\b/gi, ''))) unprovable('DDL or DML inside a CREATE TABLE cannot be checked');
    } else if ((m = new RegExp(String.raw`^CREATE\s+MATERIALIZED\s+VIEW\s+(?:IF\s+NOT\s+EXISTS\s+)?(${NAME})`, 'iu').exec(t))) {
      const call = unsafeCallIn(queryAfterAs(t.slice(m[0].length)) ?? '');
      if (call) contract(`a call to ${call} can run any SQL`);
      else created.add(`MATERIALIZED VIEW:${qualified(m[1])}`);
    } else if (own?.kind === 'INDEX') {
      if (own.unique && !isNew('TABLE', own.table) && !replacing.has(`INDEX:${own.name}`)) {
        contract('a new unique index on an existing table can refuse the previous release\'s writes');
      }
      if (!before.has(`INDEX:${own.name}`)) created.add(`INDEX:${own.name}`);
    } else if (own?.kind === 'TRIGGER') {
      if (!isNew('TABLE', own.table) && !replacing.has(`TRIGGER:${own.name}`)) {
        unprovable('a trigger on an existing table changes what the previous release\'s writes do');
      }
      // UPD1-31: every body is read, a new table's too. SQLite: BEGIN ... END;
      // Postgres: the function it runs.
      const body = dialect === 'sqlite' ? (/\bBEGIN\b([\s\S]*)\bEND\s*$/i.exec(s.scan)?.[1] ?? '') : '';
      for (const fragment of body.split(';').map((f) => f.trim()).filter(Boolean)) {
        let w;
        if (/\b(?:DROP|ALTER|RENAME|TRUNCATE|CREATE)\b/i.test(fragment)) {
          contract('a trigger body that changes the schema');
        } else if ((w = new RegExp(String.raw`^(?:DELETE\s+FROM|UPDATE(?:\s+OR\s+\w+)?|INSERT\s+OR\s+REPLACE\s+INTO|REPLACE\s+INTO)\s+(${NAME})`, 'iu').exec(fragment)) && !isNew('TABLE', w[1])) {
          unprovable(`a trigger writes ${qualified(w[1])}, which the previous release reads`);
        } else if (unsafeCallIn(fragment, true)) {
          contract(`a call to ${unsafeCallIn(fragment, true)} can run any SQL`);
        }
      }
      const fn = new RegExp(String.raw`\bEXECUTE\s+(?:FUNCTION|PROCEDURE)\s+(${NAME})\s*\(`, 'iu').exec(t);
      if (fn) unprovable(`a trigger runs ${fn[1]}, which can do anything`);
      if (!before.has(`TRIGGER:${own.name}`)) created.add(`TRIGGER:${own.name}`);
    } else if (/^CREATE\s+RULE\b/i.test(t)) {
      contract('CREATE RULE rewrites what the previous release\'s statements do');
    } else if ((m = new RegExp(String.raw`^CREATE\s+(?:RECURSIVE\s+)?VIEW\s+(?:IF\s+NOT\s+EXISTS\s+)?(${NAME})`, 'iu').exec(t))) {
      created.add(`VIEW:${qualified(m[1])}`);
    } else if (/^CREATE\s+(?:SCHEMA|SEQUENCE|TYPE|EXTENSION|DOMAIN|COLLATION|STATISTICS)\b/i.test(t)) {
      // Additive.
    } else if (/^CREATE\b/i.test(t)) {
      unprovable(`CREATE ${t.split(' ')[1]} (code or policy) cannot be checked`);
    } else if ((m = /^DROP\s+(TABLE|VIEW|MATERIALIZED\s+VIEW|INDEX|TRIGGER|SCHEMA|TYPE|FUNCTION|PROCEDURE|SEQUENCE|DOMAIN|EXTENSION|POLICY|RULE)\s+(?:CONCURRENTLY\s+)?(?:IF\s+EXISTS\s+)?(.*)$/iu.exec(t))) {
      const kind = m[1].toUpperCase().replace(/\s+/, ' ');
      const names = topLevel(m[2].replace(/\s+(?:CASCADE|RESTRICT)\s*$/i, '').replace(/\s+ON\s+.*$/i, ''));
      // Exempt: a table, view, index or trigger this file created; or an
      // index or trigger replaced later in the file by the same kind, on the
      // same table, with the same uniqueness and shape as before.
      const replaced = (name) => {
        const key = `${kind}:${lastPart(name)}`;
        const again = parsed.slice(index + 1).map((x) => definition(x.scan)).find((d) => d && `${d.kind}:${d.name}` === key);
        const same = sameDefinition(before.get(key), again);
        if (same) replacing.add(key);
        return same;
      };
      const exempt = (name) =>
        ((kind === 'TABLE' || kind === 'VIEW' || kind === 'MATERIALIZED VIEW') && isNew(kind, name)) ||
        ((kind === 'INDEX' || kind === 'TRIGGER') && (created.has(`${kind}:${lastPart(name)}`) || replaced(name)));
      if (!names.length || !names.every(exempt)) contract(`DROP ${kind} removes what the previous release may still use`);
      for (const name of names) if (kind === 'INDEX' || kind === 'TRIGGER') before.delete(`${kind}:${lastPart(name)}`);
    } else if (/^DROP\b/i.test(t)) {
      contract(`DROP ${t.split(' ')[1]} removes what the previous release may still use`);
    } else if ((m = new RegExp(String.raw`^ALTER\s+TABLE\s+(?:IF\s+EXISTS\s+)?(?:ONLY\s+)?(${NAME})\s+(.*)$`, 'isu').exec(t))) {
      if (!isNew('TABLE', m[1])) {
        const problems = [...new Set(topLevel(m[2]).map(alterTableAction).filter(Boolean))];
        for (const rule of problems) contract(rule);
      }
    } else if (/^ALTER\s+TYPE\b.*\bADD\s+VALUE\b/i.test(t)) {
      unprovable('a new value: the previous release must already read it (CONTRIBUTING.md, new values)');
    } else if (/^ALTER\b/i.test(t)) {
      contract(`ALTER ${t.split(' ')[1]} changes what the previous release uses`);
    } else if (/^TRUNCATE\b/i.test(t)) {
      contract('TRUNCATE deletes data the previous release may still read');
    } else if (/^DELETE\b/i.test(t)) {
      contract('DELETE removes rows the previous release may still read');
    } else if (/^REVOKE\b/i.test(t)) {
      contract('REVOKE takes access the previous release may need');
    } else if (/^(?:INSERT\s+OR\s+(?:REPLACE|ROLLBACK|ABORT|FAIL|IGNORE)\b|REPLACE\s+INTO\b)/i.test(t) && !/^INSERT\s+OR\s+IGNORE\b/i.test(t)) {
      contract('INSERT OR REPLACE / REPLACE INTO replaces existing rows the previous release reads');
    } else if (/^UPDATE\b/i.test(t)) {
      const set = /\bSET\b(.*?)(?:\bWHERE\b|\bFROM\b|\bRETURNING\b|$)/i.exec(t)?.[1] ?? '';
      if (/=\s*NULL\b/i.test(set)) contract('UPDATE ... = NULL erases data the previous release reads');
      else unprovable('UPDATE rewrites rows the previous release reads (an expand-only backfill must leave what it reads intact)');
    } else if (/^(?:DO|EXECUTE|CALL|PREPARE)\b/i.test(t)) {
      contract(`${t.split(' ')[0].toUpperCase()}: dynamic SQL can do anything, so it needs the release that stopped using what it changes`);
    } else if (/^COPY\b.*\bPROGRAM\b/i.test(t)) {
      contract('COPY ... PROGRAM runs a program on the database server');
    } else if (/^(?:WITH|MERGE|COPY)\b/i.test(t)) {
      unprovable(`${t.split(' ')[0].toUpperCase()} cannot be checked (compound SQL)`);
    } else if (/^(?:SET|RESET)\s+(?:(?:SESSION|LOCAL)\s+)?search_path\b/i.test(t)) {
      unprovable('SET search_path changes which objects the names after it mean');
    } else if (SAFE_KINDS.test(t)) {
      const call = /^(?:INSERT|SELECT|VALUES|SET|PRAGMA)\b/i.test(t) ? unsafeCallIn(t) : null;
      if (call) {
        contract(`a call to ${call} can run any SQL`);
      } else if (/\bON\s+CONFLICT\b.*\bDO\s+UPDATE\b/i.test(t)) {
        unprovable('an upsert (ON CONFLICT DO UPDATE) rewrites existing rows');
      } else if (HIDDEN_DDL.test(s.text.replace(/^\s*(?:INSERT|COMMENT)\b[\s\S]*?\bVALUES\b/i, ''))) {
        unprovable('a statement naming DROP, ALTER, DELETE or the like (in a string or a call) cannot be checked');
      }
    } else {
      unprovable(`an unknown statement (${t.split(' ')[0]}) is not assumed safe`);
    }
  }
  return out;
}

/** The released versions: the repository's vX.Y.Z tags (empty before the first release). */
export function releasedVersions(root) {
  const out = spawnSync('git', ['-C', root, 'tag', '-l', 'v*'], { encoding: 'utf8' });
  if (out.status !== 0) return [];
  return out.stdout.split('\n').map((t) => t.trim().replace(/^v/, '')).filter((v) => semver(v));
}

/**
 * Check every non-baseline migration. Returns a list of error strings.
 * `version` is this release (the workspace version); `releases` the
 * versions released so far (an unused-since release must be one of them,
 * once there are any).
 */
export function check({ root, version, directories = DIRECTORIES, baseline = BASELINE, releases = releasedVersions(root) }) {
  const current = semver(version);
  if (!current) return [`workspace version ${version} is not MAJOR.MINOR.PATCH`];
  const errors = [];
  for (const dir of directories) {
    const path = join(root, dir);
    if (!existsSync(path)) continue;
    const grandfathered = new Set(baseline[dir] ?? []);
    let history = '';
    for (const name of readdirSync(path).filter((n) => n.endsWith('.sql')).sort()) {
      const sql = readFileSync(join(path, name), 'utf8');
      const earlier = history;
      history += `\n${sql}\n;`;
      if (grandfathered.has(name)) continue;
      const file = relative(root, join(path, name));
      // Re-check #5: anything that looks like an annotation must be exactly one.
      sql.split('\n').forEach((line, index) => {
        if (/^\s*--/.test(line) && NEAR_ANNOTATION.test(line) && !CONTRACT.test(line) && !EXPAND_ONLY.test(line)) {
          errors.push(
            `${file}:${index + 1}: malformed annotation: write exactly \`-- contract(unused-since X.Y.Z): <reason>\` or ` +
              '`-- expand-only: <reason>`, at the start of the line.',
          );
        }
      });
      for (const f of findings(sql, dialectOf(dir), { history: earlier })) {
        const contracts = f.comments.map((c) => CONTRACT.exec(c)).filter(Boolean);
        const expands = f.comments.map((c) => EXPAND_ONLY.exec(c)).filter(Boolean);
        if (!contracts.length && expands.length) {
          if (f.kind === 'contract') {
            errors.push(`${file}:${f.line}: ${f.rule}; expand-only does not cover it: it needs \`-- contract(unused-since X.Y.Z): ...\`.`);
          } else if (DESTRUCTIVE_WORDS.test(f.text)) {
            errors.push(
              `${file}:${f.line}: expand-only cannot cover a statement that names DROP, DELETE, TRUNCATE, ALTER or RENAME ` +
                '(in its text, strings or body): it needs `-- contract(unused-since X.Y.Z): ...`.',
            );
          }
          continue;
        }
        if (!contracts.length) {
          errors.push(
            `${file}:${f.line}: ${f.rule}.\n    ${f.statement}\n    Expand/contract: keep the old shape until a release no longer uses it, then annotate the ` +
              `removal with \`-- contract(unused-since X.Y.Z): <what stopped using it>\` naming that (older) release` +
              (f.kind === 'unprovable' ? '; or, if the previous release keeps working, `-- expand-only: <why>`.' : '.'),
          );
          continue;
        }
        const [, release] = contracts.at(-1);
        const named = semver(release);
        if (!older(named, current)) {
          errors.push(
            `${file}:${f.line}: contract annotation names ${release}, but this release is ${version}: the release that stops ` +
              'using the old shape must ship before the one that removes it (two-release contract).',
          );
        } else if (releases.length && !releases.includes(release)) {
          errors.push(`${file}:${f.line}: contract annotation names ${release}, but there is no release ${release} (tags: v${releases.join(', v')}).`);
        }
      }
    }
  }
  return errors;
}

/**
 * The rollback floor of one store (`postgres`, `d1`, `execution_do`,
 * `org_do`, `webhook_do`; see STORE_DIRS): the oldest release whose code is
 * safe on its schema, i.e. the newest exact `contract(unused-since X.Y.Z)`
 * in its migrations, or `[workspace.metadata.runbook] rollback-floor` in
 * Cargo.toml when that is newer (for what migrations do not show, such as a
 * new value written for the first time). Null: any release since UPD1.
 */
export function rollbackFloor(root, store) {
  const dirs = STORE_DIRS[store];
  if (!dirs) throw new Error(`unknown store ${store}`);
  const texts = [];
  for (const dir of dirs) {
    if (!existsSync(join(root, dir))) continue;
    for (const name of readdirSync(join(root, dir)).filter((n) => n.endsWith('.sql'))) texts.push(readFileSync(join(root, dir, name), 'utf8'));
  }
  const toml = readFileSync(join(root, 'Cargo.toml'), 'utf8');
  const meta = toml.indexOf('[workspace.metadata.runbook]');
  const manual = meta >= 0 ? /^rollback-floor = "(\d+\.\d+\.\d+)"$/m.exec(toml.slice(meta))?.[1] : null;
  return annotationFloor(manual ? [...texts, `-- contract(unused-since ${manual}): workspace metadata`] : texts);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const root = join(dirname(fileURLToPath(import.meta.url)), '..');
  const version = workspaceVersion(root);
  if (process.argv[2] === 'floor') {
    console.log(rollbackFloor(root, process.argv[3] ?? 'postgres') ?? '');
    process.exit(0);
  }
  const errors = check({ root, version });
  if (errors.length) {
    console.error(`Migration safety (expand/contract, release ${version}): ${errors.length} problem(s)\n`);
    for (const e of errors) console.error(`  ${e}\n`);
    console.error('See CONTRIBUTING.md, "Migrations: expand/contract".');
    process.exit(1);
  }
  console.log(`    expand/contract: every new migration keeps release ${version}'s predecessor working`);
}
