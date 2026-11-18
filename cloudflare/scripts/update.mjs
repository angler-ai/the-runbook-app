#!/usr/bin/env node
// UPD1: update a Cloudflare deployment of The Runbook to this checkout's
// release (or a release tag), following README §0's operator steps. The
// same path serves any deployment of this edition.
//
//   npm run update -- --dry-run                 # the plan; read-only calls only
//   npm run update                              # the latest published release, asking first
//   npm run update -- --to 0.3.1 --yes          # a named release (or X.Y: the newest of that line)
//   npm run update -- --source checkout         # build and deploy this checkout as it is
//   npm run update -- --source checkout --to latest   # check the highest vX.Y.Z tag out first
//
// Sources:
//   release   (default) the GitHub Release's runbook-cloudflare-X.Y.Z.tar.gz
//             (--release-repo / RUNBOOK_RELEASE_REPO, default angler-ai/the-runbook-app;
//             GITHUB_TOKEN for a private repository), checked against its
//             SHA256SUMS, unpacked, its dependencies installed, and its own
//             scripts/update.mjs run on it (`--source bundle`): the prebuilt
//             Workers and web UI are deployed, nothing is built.
//   checkout  this checkout, built here; with --to, the tag is checked out first.
//
// Options:
//   --config FILE          the api Worker's config (default wrangler.prod.jsonc)
//   --auth-config FILE     the sign-in Worker's (default wrangler.auth.prod.jsonc)
//   --no-auth              no sign-in Worker is deployed
//   --url ORIGIN           where /v1/meta answers (default: the api config's ORIGIN)
//   --source release|checkout   (see above; `bundle` is what a release runs)
//   --release-repo OWNER/NAME   where releases are published
//   --to latest|X.Y|X.Y.Z  the release (release: default latest; checkout: check
//                          that tag out first, refusing local changes, install
//                          its dependencies and run its own update script)
//   --verify-timeout SECS  how long to wait for /v1/meta to report the release (120)
//   --restore-backup       on a failure, also restore D1 to the bookmarks taken
//                          before migrating (required below the rollback floor;
//                          Durable Object and R2 state is not restored)
//   --accept-no-rollback   update across a Durable Object class migration,
//                          which wrangler rollback cannot cross
//   --insecure-skip-verify run a release bundle without verifying its attestation
//   --attestation-repo O/N the repository the bundle must be attested by
//   --state-dir DIR        where the D1 bookmarks are saved (default .update/)
//   --redeploy             with --source checkout: deploy even when the deployed
//                          release has the same version (a test site tracking main)
//   --yes                  do not ask        --dry-run   plan only
//
// Steps (README §0, §2):
//   1. Preflight (read-only): both configs; `wrangler whoami`; the deployed
//      release (/v1/meta); the target release (Cargo.toml); refuses a
//      downgrade; the config's Durable Object migration tags include every
//      tag the release's template (wrangler.jsonc.example) has; each Worker's
//      current version (`wrangler deployments status`), for rollback; each
//      D1 database's migration state.
//   2. Build: the api Worker (deploy build), the sign-in Worker, and the web
//      UI with its _headers when the config serves assets. A failure here
//      changes nothing.
//   3. Record a D1 Time Travel bookmark per database (the restore command is
//      printed), then apply the pending D1 migrations with `wrangler d1
//      migrations apply --remote`: first the api's (each D1 binding with a
//      migrations_dir), then the sign-in Worker's tables. Files the Worker's
//      own runner already applied (it migrates a database on first use, and
//      records that in `schema_migration`) are first recorded in wrangler's
//      `d1_migrations`, so wrangler applies only what neither applied; the
//      Worker's runner in turn adopts what wrangler applied. Durable Object
//      schemas migrate inside each object on first use; class migrations
//      ride on `wrangler deploy` (the config's `migrations`).
//   4. Deploy the sign-in Worker, then the api Worker.
//   5. Verify: /v1/meta reports the release and its schema heads; discovery answers.
//   6. On a failure in 4–5: `wrangler rollback` each deployed Worker to the
//      version recorded in 1 (api first). The schema stays: every migration
//      keeps the previous release working (expand/contract, CONTRIBUTING.md),
//      and its D1 and DO runners accept a newer release's schema.
//
// Exit status: 0 updated (or already current), 1 refused before any change,
// 2 usage, 3 failed and rolled back, 4 failed and not (fully) rolled back.
//
// Uses your wrangler login (or CLOUDFLARE_API_TOKEN). Prints no secrets.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { createInterface } from 'node:readline/promises';
import { fileURLToPath } from 'node:url';

export const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repo = resolve(root, '..');
export const EXIT = { OK: 0, REFUSED: 1, USAGE: 2, ROLLED_BACK: 3, FAILED: 4 };

export class Refusal extends Error {
  constructor(message, code = EXIT.REFUSED) {
    super(message);
    this.code = code;
  }
}

// --- options ---------------------------------------------------------------

export function parseArgs(argv) {
  const opts = {
    config: 'wrangler.prod.jsonc',
    authConfig: 'wrangler.auth.prod.jsonc',
    auth: true,
    url: null,
    to: null,
    source: 'release',
    releaseRepo: process.env.RUNBOOK_RELEASE_REPO || 'angler-ai/the-runbook-app',
    resumedFrom: null,
    verifyTimeoutMs: 120_000,
    yes: false,
    dryRun: false,
    restoreBackup: false,
    acceptNoRollback: false,
    insecureSkipVerify: false,
    attestationRepo: process.env.RUNBOOK_ATTESTATION_REPO || null,
    stateDir: null,
    redeploy: false,
  };
  const value = (i, name) => {
    if (i + 1 >= argv.length) throw new Refusal(`${name} needs a value`, EXIT.USAGE);
    return argv[i + 1];
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--config') opts.config = value(i++, a);
    else if (a === '--auth-config') opts.authConfig = value(i++, a);
    else if (a === '--no-auth') opts.auth = false;
    else if (a === '--url') opts.url = value(i++, a).replace(/\/+$/, '');
    else if (a === '--to') opts.to = value(i++, a).replace(/^v/, '');
    else if (a === '--source') opts.source = value(i++, a);
    else if (a === '--release-repo') opts.releaseRepo = value(i++, a);
    else if (a === '--resumed-from') opts.resumedFrom = value(i++, a);
    else if (a === '--verify-timeout') {
      const secs = Number(value(i++, a));
      if (!Number.isInteger(secs) || secs < 1) throw new Refusal('--verify-timeout takes whole seconds', EXIT.USAGE);
      opts.verifyTimeoutMs = secs * 1000;
    } else if (a === '--yes' || a === '-y') opts.yes = true;
    else if (a === '--dry-run') opts.dryRun = true;
    else if (a === '--restore-backup') opts.restoreBackup = true;
    else if (a === '--accept-no-rollback') opts.acceptNoRollback = true;
    else if (a === '--insecure-skip-verify') opts.insecureSkipVerify = true;
    else if (a === '--redeploy') opts.redeploy = true;
    else if (a === '--attestation-repo') opts.attestationRepo = value(i++, a);
    else if (a === '--state-dir') opts.stateDir = value(i++, a);
    else if (a === '--help' || a === '-h') opts.help = true;
    else throw new Refusal(`unknown option ${a} (see --help)`, EXIT.USAGE);
  }
  if (!['release', 'checkout', 'bundle'].includes(opts.source)) throw new Refusal('--source must be release or checkout', EXIT.USAGE);
  if (opts.to && opts.to !== 'latest' && !/^\d+\.\d+(\.\d+)?$/.test(opts.to)) {
    throw new Refusal('--to must be latest, X.Y or X.Y.Z', EXIT.USAGE);
  }
  if (!/^[\w.-]+\/[\w.-]+$/.test(opts.releaseRepo)) throw new Refusal('--release-repo must be OWNER/NAME', EXIT.USAGE);
  return opts;
}

// --- versions --------------------------------------------------------------

export function semver(text) {
  const m = /^(\d+)\.(\d+)\.(\d+)$/.exec(text ?? '');
  return m ? m.slice(1).map(Number) : null;
}
/** -1, 0 or 1; `unknown` (a release before UPD1) is older than all. */
export function compare(a, b) {
  if (a === b) return 0;
  if (a === 'unknown') return -1;
  if (b === 'unknown') return 1;
  const [x, y] = [semver(a), semver(b)];
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
  return 0;
}
export function cargoVersion(toml) {
  const section = toml.slice(toml.indexOf('[workspace.package]'));
  return /^version\s*=\s*"([^"]+)"/m.exec(section)?.[1] ?? null;
}
export function latestTag(tags, line = null) {
  return tags
    .filter((t) => semver(t.replace(/^v/, '')) && t.startsWith('v'))
    .filter((t) => !line || t.slice(1).startsWith(`${line}.`))
    .reduce((best, t) => (!best || compare(t.slice(1), best.slice(1)) > 0 ? t : best), null);
}

/** The schema heads a checkout's code migrates to (what /v1/meta reports). */
export function schemaHeads(dir = repo) {
  const head = (sub) => {
    const files = readdirSync(join(dir, 'cloudflare', 'migrations', sub)).filter((f) => /^\d{4}_.*\.sql$/.test(f));
    return Math.max(0, ...files.map((f) => Number(f.slice(0, 4))));
  };
  return { d1: head('d1'), execution_do: head('do'), org_do: head('orgdo'), webhook_do: head('webhookdo') };
}

// UPD1 re-check #5: the one annotation grammar (scripts/check-migrations.mjs
// enforces it and rejects near misses; runbook-server's build.rs reads the
// same): exactly `-- contract(unused-since X.Y.Z): <reason>` at column 0.
const CONTRACT = /^-- contract\(unused-since (\d+\.\d+\.\d+)\): \S/;

/** The newest exact `contract(unused-since X.Y.Z)` in these texts, or null. */
export function annotationFloor(texts) {
  let floor = null;
  for (const text of texts) {
    for (const line of text.split('\n')) {
      const m = CONTRACT.exec(line);
      if (m && (!floor || compare(floor, m[1]) < 0)) floor = m[1];
    }
  }
  return floor;
}

/** The Cloudflare edition's stores and the directories of their migrations
 * (the sign-in service's D1 tables are applied with the api's D1). */
export const STORES = {
  d1: ['cloudflare/migrations/d1', 'auth/drizzle/sqlite'],
  execution_do: ['cloudflare/migrations/do'],
  org_do: ['cloudflare/migrations/orgdo'],
  webhook_do: ['cloudflare/migrations/webhookdo'],
};

/**
 * UPD1 review #2, re-check #2: this checkout's rollback floor per store:
 * the newest `contract(unused-since X.Y.Z)` in that store's migrations, or
 * `[workspace.metadata.runbook] rollback-floor` when newer. Null: any
 * release since UPD1 is safe on that store's schema. (A release bundle
 * carries these in RELEASE.json.)
 */
export function rollbackFloor(dir = repo) {
  const cargo = join(dir, 'Cargo.toml');
  let manual = null;
  if (existsSync(cargo)) {
    const toml = readFileSync(cargo, 'utf8');
    const at = toml.indexOf('[workspace.metadata.runbook]');
    if (at >= 0) manual = /^rollback-floor = "(\d+\.\d+\.\d+)"$/m.exec(toml.slice(at))?.[1] ?? null;
  }
  const out = {};
  for (const [store, dirs] of Object.entries(STORES)) {
    const texts = [];
    for (const sub of dirs) {
      const path = join(dir, sub);
      if (!existsSync(path)) continue;
      for (const f of readdirSync(path).filter((n) => n.endsWith('.sql'))) texts.push(readFileSync(join(path, f), 'utf8'));
    }
    if (manual) texts.push(`-- contract(unused-since ${manual}): workspace metadata`);
    out[store] = annotationFloor(texts);
  }
  return out;
}

// --- preflight -------------------------------------------------------------

const SAFE_NAME = /^[\w.-]+$/;

/** JSON rows of one `wrangler d1 execute --json --command` answer. */
function rows(stdout) {
  const parsed = JSON.parse(stdout);
  return (Array.isArray(parsed) ? parsed : [parsed]).flatMap((r) => r.results ?? []);
}

async function d1State(binding, api, deps) {
  const query = async (sql) => {
    const out = await deps.wrangler(['d1', 'execute', binding.binding, '--remote', '--json', '--command', sql, '-c', api.path], { capture: true });
    if (out.status !== 0) throw new Refusal(`cannot read ${binding.binding}'s migration state (wrangler d1 execute failed)`);
    return rows(out.stdout);
  };
  const table = binding.migrations_table ?? 'd1_migrations';
  if (!SAFE_NAME.test(table)) throw new Refusal(`unexpected migrations_table ${table}`);
  const tables = new Set(
    (await query(`SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('schema_migration', '${table}')`)).map((r) => r.name),
  );
  const runner = tables.has('schema_migration') ? (await query('SELECT version FROM schema_migration')).map((r) => Number(r.version)) : [];
  const wrangler = tables.has(table) ? (await query(`SELECT name FROM ${table}`)).map((r) => r.name) : [];
  const dir = isAbsolute(binding.migrations_dir) ? binding.migrations_dir : resolve(dirname(api.path), binding.migrations_dir);
  const files = readdirSync(dir).filter((f) => /^\d{4}_[\w.-]*\.sql$/.test(f)).sort();
  // Applied by the Worker's runner but unknown to wrangler: record them.
  const record = files.filter((f) => runner.includes(Number(f.slice(0, 4))) && !wrangler.includes(f));
  const pending = files.filter((f) => !runner.includes(Number(f.slice(0, 4))) && !wrangler.includes(f));
  return { binding: binding.binding, database: binding.database_name, table, record, pending };
}

/** Everything the plan is made from. Read-only. */
export async function gather(opts, deps) {
  const base = deps.root ?? root;
  const top = resolve(base, '..');
  const cfgPath = (p) => (isAbsolute(p) ? p : resolve(base, p));
  const load = (p, what) => {
    const path = cfgPath(p);
    if (!deps.exists(path)) {
      throw new Refusal(`no ${what} config at ${path}: copy ${what === 'api' ? 'wrangler.jsonc.example' : 'wrangler.auth.jsonc.example'} and fill it in (README §0)`);
    }
    const config = deps.readConfig(path);
    if (opts.source !== 'bundle') return { path, config };
    // A release bundle: the operator's config, beside the bundle's built
    // files (its relative paths resolve there), without its build command.
    delete config.build;
    const copy = join(base, `wrangler.${what}.release.json`);
    deps.writeFile(copy, `${JSON.stringify(config, null, 2)}\n`);
    return { path: copy, config };
  };
  const api = load(opts.config, 'api');
  const auth = opts.auth ? load(opts.authConfig, 'auth') : null;

  if ((await deps.wrangler(['whoami', '--json'], { capture: true })).status !== 0) {
    throw new Refusal('wrangler is not logged in: run `npx wrangler login` (or set CLOUDFLARE_API_TOKEN)');
  }
  const origin = opts.url ?? api.config.vars?.ORIGIN;
  if (!origin) throw new Refusal('no origin: pass --url or set vars.ORIGIN in the api config');
  const meta = await deps.fetchJson(`${origin}/v1/meta`);
  if (!meta) throw new Refusal(`${origin}/v1/meta does not answer: is the api Worker deployed?`);
  const deployed = { version: semver(meta.version) ? meta.version : 'unknown', schema: meta.schema ?? {} };

  const release = join(top, 'RELEASE.json');
  const manifest = deps.exists(release) ? JSON.parse(deps.readFile(release)) : null;
  const version = manifest ? manifest.version : cargoVersion(deps.readFile(join(top, 'Cargo.toml')));
  // UPD1-31: a release states its floors; a manifest without them is not
  // read as "none" (an older or damaged bundle could hide a floor).
  if (manifest && (typeof manifest.rollback_floor !== 'object' || manifest.rollback_floor === null)) {
    throw new Refusal(`${release}: RELEASE.json has no rollback_floor, so the update cannot tell which releases are safe to roll back to`);
  }
  const floors = manifest ? manifest.rollback_floor : deps.rollbackFloor(top);
  // Re-check #2: D1's floor can be met by restoring D1; a Durable Object
  // floor cannot (a D1 restore does not undo an object's schema).
  const newest = (list) => list.filter(Boolean).reduce((a, b) => (!a || compare(a, b) < 0 ? b : a), null);
  const doStores = ['execution_do', 'org_do', 'webhook_do'].filter((st) => floors[st]);
  const target = {
    version,
    heads: deps.schemaHeads(top),
    floor: floors.d1 ?? null,
    doFloor: newest(doStores.map((st) => floors[st])),
    doFloorStores: doStores,
    floors,
  };
  // The release each Durable Object class migration tag arrived in (a
  // rollback cannot cross one; Cloudflare's Rollbacks docs).
  const classesFile = join(top, 'cloudflare', 'migrations', 'do-classes.json');
  const doClasses = JSON.parse(deps.readFile(classesFile));
  if (!semver(target.version)) throw new Refusal(`Cargo.toml's workspace version ${target.version} is not MAJOR.MINOR.PATCH`);

  // Durable Object classes: every tag the release's template declares.
  const template = deps.readConfig(join(base, 'wrangler.jsonc.example')).migrations ?? [];
  const have = (api.config.migrations ?? []).map((m) => JSON.stringify(m));
  const missingTags = template.filter((m) => !have.includes(JSON.stringify(m)));

  const previous = {};
  for (const [name, worker] of [['auth', auth], ['api', api]]) {
    if (!worker) continue;
    const out = await deps.wrangler(['deployments', 'status', '--json', '-c', worker.path], { capture: true });
    const version = out.status === 0 ? JSON.parse(out.stdout).versions?.[0]?.version_id : null;
    if (!version) throw new Refusal(`the ${name} Worker has no deployment to roll back to: deploy it the first time with README §0`);
    previous[name] = version;
  }

  const d1 = [];
  for (const binding of api.config.d1_databases ?? []) {
    if (binding.migrations_dir) d1.push(await d1State(binding, api, deps));
  }
  if (!d1.length) throw new Refusal('the api config has no D1 database with a migrations_dir (README §0)');
  return {
    api, auth, origin, deployed, target, missingTags, previous, d1, doClasses,
    prebuilt: opts.source === 'bundle',
    unverified: opts.source === 'bundle' && opts.insecureSkipVerify,
  };
}

// --- the plan --------------------------------------------------------------

const wranglerStep = (id, title, args, extra = {}) => ({ id, title, kind: 'wrangler', args, ...extra });

/** Refusals, or the ordered steps. Pure. */
export function decide(facts, opts = {}) {
  const { api, auth, deployed, target, missingTags, d1 } = facts;
  if (compare(target.version, deployed.version) < 0) {
    throw new Refusal(`${target.version} is older than the deployed ${deployed.version}: downgrades are refused (use wrangler rollback)`);
  }
  if (compare(target.version, deployed.version) === 0 && !(opts.redeploy && opts.source === 'checkout')) {
    return { upToDate: true, steps: [] };
  }
  if (missingTags.length) {
    throw new Refusal(
      `the api config lacks Durable Object migrations this release needs; add to its "migrations":\n${missingTags
        .map((m) => `  ${JSON.stringify(m)}`)
        .join('\n')}`,
    );
  }
  // A class migration the deployed release does not have: wrangler rollback
  // cannot cross it, so there is no automatic rollback (say so up front).
  const crossed = (facts.doClasses ?? []).filter((c) => c.release !== '0.0.0' && compare(c.release, deployed.version) > 0);
  if (crossed.length && !opts.acceptNoRollback) {
    throw new Refusal(
      `${target.version} brings Durable Object class migration ${crossed.map((c) => c.tag).join(', ')}: Cloudflare cannot roll back across ` +
        'a class migration, so a failed update could not be rolled back automatically. Pass --accept-no-rollback to update anyway.',
    );
  }
  // Below a Durable Object floor the deployed release's code is not safe on
  // an object's new schema, and a D1 restore does not change that: no
  // rollback is possible at all.
  const belowDoFloor = target.doFloor && compare(deployed.version, target.doFloor) < 0;
  if (belowDoFloor && !opts.acceptNoRollback) {
    throw new Refusal(
      `${deployed.version} is below ${target.version}'s Durable Object rollback floor ${target.doFloor} (${target.doFloorStores.join(', ')}): ` +
        'its code is not safe on the objects\' new schema, and restoring D1 does not undo that, so a failed update could not be rolled back. ' +
        'Pass --accept-no-rollback to update anyway.',
    );
  }
  // Below the D1 floor the deployed release's code is not safe on the new
  // D1 schema: a Worker rollback alone would not do.
  const belowFloor = !belowDoFloor && target.floor && compare(deployed.version, target.floor) < 0;
  if (belowFloor && !opts.restoreBackup) {
    throw new Refusal(
      `${deployed.version} is below ${target.version}'s rollback floor ${target.floor}: its code is not safe on the new schema. ` +
        'Pass --restore-backup, so that a failure also restores D1 to the bookmarks taken now (Durable Objects and R2 are not restored).',
    );
  }
  const steps = [];
  if (!facts.prebuilt) steps.push({ id: 'build:api', title: 'build the api Worker (deploy build)', kind: 'run', cmd: [process.execPath, 'scripts/build-worker.mjs', 'worker'], cwd: root });
  if (auth && !facts.prebuilt) steps.push({ id: 'build:auth', title: 'build the sign-in Worker', kind: 'run', cmd: [process.execPath, 'scripts/build-worker.mjs', 'auth'], cwd: root });
  if (api.config.assets && !facts.prebuilt) {
    steps.push(
      { id: 'build:wasm', title: 'build the browser compiler', kind: 'run', cmd: [process.execPath, 'scripts/build-wasm.mjs'], cwd: join(repo, 'web') },
      { id: 'build:web', title: 'build the web UI', kind: 'run', cmd: ['npm', 'run', 'build'], cwd: join(repo, 'web') },
      { id: 'build:assets', title: 'prepare build-assets/ (web UI and _headers)', kind: 'run', cmd: [process.execPath, 'scripts/prepare-assets.mjs'], cwd: root },
    );
  }
  const databases = [...new Set(d1.map((d) => d.database))];
  for (const database of databases) {
    steps.push(wranglerStep(`bookmark:${database}`, `record a D1 Time Travel bookmark of ${database}`, ['d1', 'time-travel', 'info', database, '--json', '-c', api.path], { bookmark: database, config: api.path }));
  }
  const authDatabase = auth?.config.d1_databases?.find((d) => d.binding === 'DB')?.database_name;
  if (authDatabase && !databases.includes(authDatabase)) {
    steps.push(wranglerStep(`bookmark:${authDatabase}`, `record a D1 Time Travel bookmark of ${authDatabase} (sign-in)`, ['d1', 'time-travel', 'info', authDatabase, '--json', '-c', auth.path], { bookmark: authDatabase, config: auth.path }));
  }
  for (const db of d1) {
    if (db.record.length) {
      if (!db.record.every((f) => SAFE_NAME.test(f))) throw new Refusal(`unexpected migration file names in ${db.binding}`);
      const sql =
        `CREATE TABLE IF NOT EXISTS ${db.table}(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE, applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL); ` +
        `INSERT OR IGNORE INTO ${db.table} (name) VALUES ${db.record.map((f) => `('${f}')`).join(', ')};`;
      steps.push(
        wranglerStep(`record:${db.binding}`, `record ${db.record.length} file(s) the Worker's runner applied in ${db.binding}'s ${db.table}`, [
          'd1', 'execute', db.binding, '--remote', '--command', sql, '-c', api.path,
        ], { migrates: true }),
      );
    }
    steps.push(
      wranglerStep(`migrate:${db.binding}`, `apply ${db.pending.length ? db.pending.join(', ') : 'no pending migrations'} to ${db.binding}`, [
        'd1', 'migrations', 'apply', db.binding, '--remote', '-c', api.path,
      ], { migrates: true }),
    );
  }
  if (auth) {
    steps.push(wranglerStep('migrate:auth', "apply the sign-in Worker's pending D1 migrations", ['d1', 'migrations', 'apply', 'DB', '--remote', '-c', auth.path], { migrates: true }));
    steps.push(wranglerStep('deploy:auth', 'deploy the sign-in Worker', ['deploy', '-c', auth.path], { deploys: 'auth' }));
  }
  steps.push(wranglerStep('deploy:api', 'deploy the api Worker', ['deploy', '-c', api.path], { deploys: 'api' }));
  steps.push({ id: 'verify', title: `verify /v1/meta reports ${target.version} and its schema`, kind: 'verify' });
  return { upToDate: false, steps, noRollback: crossed.length > 0 || Boolean(belowDoFloor), restoreOnFailure: Boolean(opts.restoreBackup) };
}

// --- running it ------------------------------------------------------------

async function verify(facts, opts, deps) {
  const deadline = Date.now() + opts.verifyTimeoutMs;
  let last = 'no answer';
  for (;;) {
    const meta = await deps.fetchJson(`${facts.origin}/v1/meta`);
    if (meta?.version === facts.target.version) {
      const wrong = Object.entries(facts.target.heads).filter(([k, v]) => meta.schema?.[k] !== v);
      if (!wrong.length) {
        if (!(await deps.fetchJson(`${facts.origin}/.well-known/runbook`))) return 'discovery does not answer';
        deps.log(`    /v1/meta: release ${meta.version}, schema ${JSON.stringify(meta.schema)}`);
        return null;
      }
      last = `schema ${JSON.stringify(meta.schema)}, expected ${JSON.stringify(facts.target.heads)}`;
    } else {
      last = meta ? `release ${meta.version ?? 'unknown'}` : 'no answer';
    }
    if (Date.now() >= deadline) return `/v1/meta reports ${last}, not release ${facts.target.version}`;
    await deps.sleep(2_000);
  }
}

function printPlan(facts, plan, deps) {
  const log = deps.log;
  log(`==> ${facts.origin}: ${facts.deployed.version} -> ${facts.target.version}`);
  log(`    rollback points: ${Object.entries(facts.previous).map(([w, v]) => `${w} ${v}`).join(', ')}`);
  for (const db of facts.d1) {
    log(`    ${db.binding} (${db.database}): ${db.pending.length} pending (${db.pending.join(', ') || 'none'})` +
      (db.record.length ? `; ${db.record.length} applied by the Worker to record for wrangler` : ''));
  }
  log('==> Plan');
  plan.steps.forEach((s, i) => log(`    ${i + 1}. ${s.title}`));
  for (const [store, floor] of Object.entries(facts.target.floors ?? {})) {
    if (floor) log(`    rollback floor ${store}: ${floor} (older code is not safe on this schema)`);
  }
  if (facts.unverified) log('    provenance: NOT verified (--insecure-skip-verify)');
  if (plan.noRollback) {
    log('    on a failure: no automatic rollback (a Durable Object class migration or rollback floor; --accept-no-rollback)');
  } else {
    log('    on a failure while deploying or verifying: wrangler rollback each deployed Worker (api, then auth); ' +
      (plan.restoreOnFailure ? 'and restore D1 to the bookmarks (--restore-backup; Durable Objects and R2 are not restored)' : 'the schema stays'));
  }
}

/** Run the steps; returns an exit code. */
export async function execute(facts, plan, opts, deps) {
  const deployed = [];
  const bookmarks = [];
  const stateDir = opts.stateDir ?? join(deps.root ?? root, '.update');
  const bookmarkFile = join(stateDir, `bookmarks-${new Date().toISOString().replace(/[:.]/g, '-')}.json`);
  const restoreD1 = async () => {
    let ok = true;
    deps.log('==> Restoring D1 to the bookmarks taken before the update (--restore-backup)');
    deps.log('    Durable Object and R2 state is not restored: runs, delivery logs and blobs written since stay, and may refer to rows the restore removes');
    for (const b of bookmarks) {
      const out = await deps.wrangler(['d1', 'time-travel', 'restore', b.database, `--bookmark=${b.bookmark}`, '-c', b.config], { env: { CI: '1' } });
      if (out.status !== 0) {
        ok = false;
        deps.log(`error: restoring ${b.database} failed; restore it by hand: npx wrangler d1 time-travel restore ${b.database} --bookmark=${b.bookmark}`);
      }
    }
    return ok;
  };
  const rollback = async (reason) => {
    deps.log(`error: ${reason}`);
    if (plan.noRollback) {
      deps.log('    no automatic rollback: a Durable Object class migration or rollback floor (--accept-no-rollback). Fix forward, or redeploy by hand.');
      restoreHint();
      return EXIT.FAILED;
    }
    if (!deployed.length && !plan.restoreOnFailure) return EXIT.FAILED;
    if (!opts.yes && !(await deps.confirm('Roll back the deployed Workers?'))) {
      for (const w of deployed.reverse()) deps.log(`    to roll back: npx wrangler rollback ${facts.previous[w]} -c ${facts[w].path}`);
      restoreHint();
      return EXIT.FAILED;
    }
    let ok = true;
    for (const w of [...deployed].reverse()) {
      deps.log(`==> Rolling the ${w} Worker back to ${facts.previous[w]}`);
      const out = await deps.wrangler(['rollback', facts.previous[w], '-c', facts[w].path, '--message', `update.mjs: ${facts.target.version} failed`, '--yes'], {});
      if (out.status !== 0) ok = false;
    }
    if (plan.restoreOnFailure && !(await restoreD1())) ok = false;
    if (ok) {
      deps.log(`==> Rolled back to ${facts.deployed.version}${plan.restoreOnFailure ? ' and its D1 data' : '; the schema stays (expand/contract)'}`);
    } else {
      deps.log('error: a rollback step failed; see wrangler deployments list');
      restoreHint();
    }
    return ok ? EXIT.ROLLED_BACK : EXIT.FAILED;
  };
  const restoreHint = () => {
    for (const b of bookmarks) deps.log(`    D1 before this update: npx wrangler d1 time-travel restore ${b.database} --bookmark=${b.bookmark} -c ${b.config}`);
    if (bookmarks.length) deps.log(`    (also in ${bookmarkFile}; a D1 restore does not restore Durable Object or R2 state)`);
  };

  for (const step of plan.steps) {
    deps.log(`==> ${step.title}`);
    if (step.kind === 'run') {
      if (deps.run(step.cmd, step.cwd) !== 0) {
        deps.log(`error: ${step.id} failed; nothing was changed`);
        return EXIT.REFUSED;
      }
      continue;
    }
    if (step.kind === 'verify') {
      const problem = await verify(facts, opts, deps);
      if (problem) {
        restoreHint();
        return rollback(problem);
      }
      continue;
    }
    const out = await deps.wrangler(step.args, { capture: Boolean(step.bookmark), env: step.migrates ? { CI: '1' } : {} });
    if (out.status !== 0) {
      if (step.bookmark) {
        deps.log(`error: no Time Travel bookmark for ${step.bookmark}; nothing was changed`);
        return EXIT.REFUSED;
      }
      if (step.migrates) {
        deps.log(`error: ${step.id} failed.`);
        if (plan.restoreOnFailure) return (await restoreD1()) ? EXIT.ROLLED_BACK : EXIT.FAILED;
        restoreHint();
        deps.log('    The running release keeps working on a partly migrated schema (expand/contract); fix the cause and run the update again.');
        return EXIT.FAILED;
      }
      return rollback(`${step.id} failed`);
    }
    // A Worker counts as deployed (to roll back) once its deploy succeeded.
    if (step.deploys) deployed.push(step.deploys);
    if (step.bookmark) {
      let bookmark = null;
      try {
        bookmark = JSON.parse(out.stdout).bookmark;
      } catch {}
      if (typeof bookmark !== 'string' || !bookmark.trim()) {
        deps.log(`error: no Time Travel bookmark for ${step.bookmark} (wrangler answered without one); nothing was changed`);
        return EXIT.REFUSED;
      }
      bookmarks.push({ database: step.bookmark, bookmark, config: step.config });
      deps.mkdirp(stateDir);
      deps.writeFile(bookmarkFile, `${JSON.stringify(bookmarks, null, 2)}\n`);
      deps.log(`    restore command: npx wrangler d1 time-travel restore ${step.bookmark} --bookmark=${bookmark} -c ${step.config} (saved in ${bookmarkFile})`);
    }
  }
  deps.log(`==> Updated ${facts.deployed.version} -> ${facts.target.version}${facts.unverified ? ' (NOT verified: --insecure-skip-verify)' : ''}`);
  restoreHint();
  return EXIT.OK;
}

// --- the release source: download, verify, unpack, run its own script -------

/** The operator's arguments for a handed-over run: config paths made
 * absolute, and the options the handover itself decides dropped. */
function forwarded(argv, base) {
  const out = [];
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (['--to', '--source', '--release-repo', '--resumed-from'].includes(a)) i++;
    else if (a === '--config' || a === '--auth-config' || a === '--state-dir') out.push(a, resolve(base, argv[++i]));
    else out.push(a);
  }
  return out;
}

async function releaseAndResume(opts, argv, deps) {
  const repoUrl = `https://api.github.com/repos/${opts.releaseRepo}`;
  const releases = await deps.fetchJson(`${repoUrl}/releases?per_page=100`, deps.githubHeaders('application/vnd.github+json'));
  if (!Array.isArray(releases)) {
    throw new Refusal(
      `cannot list the releases of ${opts.releaseRepo} (a private repository needs GITHUB_TOKEN; ` +
        'if none is published yet, deploy a checkout with --source checkout)',
    );
  }
  const published = releases.filter((r) => !r.draft && !r.prerelease && /^v\d+\.\d+\.\d+$/.test(r.tag_name ?? ''));
  const names = published.map((r) => r.tag_name);
  const to = opts.to ?? 'latest';
  const tag = to === 'latest' ? latestTag(names) : semver(to) ? `v${to}` : latestTag(names, to);
  const chosen = published.find((r) => r.tag_name === tag);
  if (!chosen) throw new Refusal(`no published release ${to === 'latest' ? '' : `for ${to} `}in ${opts.releaseRepo}`);
  const version = tag.slice(1);
  const name = `runbook-cloudflare-${version}.tar.gz`;
  const asset = (n) => (chosen.assets ?? []).find((a) => a.name === n);
  if (!asset(name) || !asset('SHA256SUMS')) throw new Refusal(`release ${tag} has no ${name} and SHA256SUMS`);
  deps.log(`==> Release ${tag} of ${opts.releaseRepo}`);
  const dir = deps.tempDir();
  try {
    const octet = deps.githubHeaders('application/octet-stream');
    const sums = (await deps.download(asset('SHA256SUMS').url, octet)).toString('utf8');
    const bytes = await deps.download(asset(name).url, octet);
    const expected = sums
      .split('\n')
      .map((l) => l.trim().split(/\s+/))
      .find(([, file]) => file === name)?.[0];
    const actual = createHash('sha256').update(bytes).digest('hex');
    if (!expected || expected !== actual) throw new Refusal(`${name} does not match the release's SHA256SUMS: not used`);
    deps.log(`    ${name}: sha256 ${actual} (matches SHA256SUMS)`);
    const archive = join(dir, name);
    deps.writeFile(archive, bytes);
    // UPD1 review #4: the bundle's build provenance, before anything in it runs.
    const attester = opts.attestationRepo ?? opts.releaseRepo;
    if (opts.insecureSkipVerify) {
      deps.log(`warning: --insecure-skip-verify: ${name} is NOT verified`);
    } else if (!deps.hasGh()) {
      throw new Refusal('verifying the release needs the GitHub CLI (gh attestation verify); install gh, or pass --insecure-skip-verify');
    } else {
      // Re-check #1: pinned to the release workflow and to this release's
      // tag, not only the repository (any workflow there can attest, and a
      // tag on another commit runs that commit's release.yml).
      const pins = { signerWorkflow: `${attester}/.github/workflows/release.yml`, sourceRef: `refs/tags/${tag}` };
      const verified = deps.verifyAttestation(archive, attester, pins);
      if (verified.status !== 0) {
        throw new Refusal(
          `${name} has no valid build attestation from ${pins.signerWorkflow} at ${pins.sourceRef} (gh attestation verify)` +
            (verified.stderr?.trim() ? `:\n${verified.stderr.trim()}` : ''),
        );
      }
      deps.log(`    provenance: attested by ${pins.signerWorkflow} at ${pins.sourceRef}`);
    }
    if (deps.run(['tar', '-xzf', archive, '-C', dir], dir) !== 0) throw new Refusal(`cannot unpack ${name}`);
    const top = join(dir, `runbook-cloudflare-${version}`);
    const manifest = join(top, 'RELEASE.json');
    if (!deps.exists(manifest) || JSON.parse(deps.readFile(manifest)).version !== version) {
      throw new Refusal(`${name} is not release ${version} (RELEASE.json)`);
    }
    const bundle = join(top, 'cloudflare');
    if (deps.run(['npm', 'ci', '--ignore-scripts', '--no-audit', '--no-fund'], bundle) !== 0) {
      throw new Refusal(`npm ci in the ${tag} bundle failed`);
    }
    const base = deps.root ?? root;
    const args = forwarded(argv, base);
    if (!args.includes('--config')) args.push('--config', resolve(base, opts.config));
    if (opts.auth && !args.includes('--auth-config')) args.push('--auth-config', resolve(base, opts.authConfig));
    if (!args.includes('--state-dir')) args.push('--state-dir', join(base, '.update'));
    const code = deps.run([process.execPath, join(bundle, 'scripts', 'update.mjs'), '--source', 'bundle', '--resumed-from', tag, ...args], bundle);
    if (opts.insecureSkipVerify) deps.log(`warning: release ${tag} was NOT verified (--insecure-skip-verify)`);
    return code;
  } finally {
    deps.removeDir(dir);
  }
}

// --- checkout --to: check the release out and run its own script -------------

async function checkoutAndResume(opts, argv, deps) {
  if (deps.git(['status', '--porcelain', '--untracked-files=no']).stdout.trim()) {
    throw new Refusal('the checkout has local changes to tracked files: commit or stash them first');
  }
  const tags = deps.git(['tag', '-l', 'v*']).stdout.split('\n').filter(Boolean);
  const tag = opts.to === 'latest' ? latestTag(tags) : semver(opts.to) ? `v${opts.to}` : latestTag(tags, opts.to);
  if (!tag || !tags.includes(tag)) throw new Refusal(opts.to === 'latest' ? 'no vX.Y.Z release tags' : `no release tag for ${opts.to}`);
  const version = cargoVersion(deps.git(['show', `${tag}:Cargo.toml`]).stdout);
  if (version !== tag.slice(1)) throw new Refusal(`${tag}'s Cargo.toml says ${version}`);
  const previous = deps.git(['symbolic-ref', '-q', '--short', 'HEAD']).stdout.trim() || deps.git(['rev-parse', 'HEAD']).stdout.trim();
  deps.log(`==> ${tag} (from ${previous})`);
  if (opts.dryRun) {
    deps.log(`    would check out ${tag}, run npm ci in cloudflare/, auth/ and web/, then its scripts/update.mjs --dry-run`);
    return EXIT.OK;
  }
  if (deps.git(['checkout', '--quiet', '--detach', tag]).status !== 0) throw new Refusal(`git checkout ${tag} failed`);
  for (const dir of ['cloudflare', 'auth', 'web']) {
    if (deps.run(['npm', 'ci', '--ignore-scripts', '--no-audit', '--no-fund'], join(repo, dir)) !== 0) {
      deps.git(['checkout', '--quiet', previous]);
      throw new Refusal(`npm ci in ${dir}/ failed; ${previous} is checked out again`);
    }
  }
  const rest = [];
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--to') i++;
    else rest.push(argv[i]);
  }
  const code = deps.run([process.execPath, join(root, 'scripts', 'update.mjs'), '--resumed-from', previous, ...rest], root);
  if (code !== EXIT.OK) {
    deps.git(['checkout', '--quiet', previous]);
    deps.log(`    ${previous} is checked out again`);
  }
  return code;
}

// --- main ------------------------------------------------------------------

/** Reads JSONC Wrangler configs with wrangler's own reader. */
export async function wranglerConfigReader() {
  const wrangler = await import('wrangler');
  return (path) => structuredClone(wrangler.experimental_readRawConfig({ config: path }).rawConfig);
}

export function realDeps(readConfig) {
  const wranglerBin = join(root, 'node_modules', 'wrangler', 'bin', 'wrangler.js');
  return {
    exists: existsSync,
    readFile: (p) => readFileSync(p, 'utf8'),
    readConfig,
    root,
    writeFile: (p, text) => writeFileSync(p, text, { mode: 0o600 }),
    schemaHeads: (dir) => schemaHeads(dir ?? repo),
    wrangler: (args, { capture = false, env = {} } = {}) => {
      const out = spawnSync(process.execPath, [wranglerBin, ...args], {
        cwd: root,
        env: { ...process.env, WRANGLER_SEND_METRICS: 'false', ...env },
        encoding: 'utf8',
        stdio: capture ? ['ignore', 'pipe', 'pipe'] : 'inherit',
      });
      return { status: out.status ?? 1, stdout: out.stdout ?? '', stderr: out.stderr ?? '' };
    },
    run: (cmd, cwd) => spawnSync(cmd[0], cmd.slice(1), { cwd, stdio: 'inherit' }).status ?? 1,
    git: (args) => {
      const out = spawnSync('git', ['-C', repo, ...args], { encoding: 'utf8' });
      return { status: out.status ?? 1, stdout: out.stdout ?? '' };
    },
    fetchJson: async (url, headers = {}) => {
      try {
        const response = await fetch(url, { headers, signal: AbortSignal.timeout(10_000), redirect: 'manual' });
        return response.ok ? await response.json() : null;
      } catch {
        return null;
      }
    },
    // GitHub sends release assets through a redirect to its storage; fetch
    // drops the Authorization header when the redirect leaves the origin.
    download: async (url, headers) => {
      const response = await fetch(url, { headers, signal: AbortSignal.timeout(300_000), redirect: 'follow' });
      if (!response.ok) throw new Refusal(`download failed (HTTP ${response.status}): ${url}`);
      return Buffer.from(await response.arrayBuffer());
    },
    githubHeaders: (accept) => ({
      accept,
      'user-agent': 'runbook-update',
      'x-github-api-version': '2022-11-28',
      ...(process.env.GITHUB_TOKEN ? { authorization: `Bearer ${process.env.GITHUB_TOKEN}` } : {}),
    }),
    tempDir: () => mkdtempSync(join(tmpdir(), 'runbook-release-')),
    mkdirp: (dir) => mkdirSync(dir, { recursive: true, mode: 0o700 }),
    rollbackFloor: (dir) => rollbackFloor(dir ?? repo),
    hasGh: () => spawnSync('gh', ['--version'], { stdio: 'ignore' }).status === 0,
    verifyAttestation: (file, attester, { signerWorkflow, sourceRef }) => {
      const out = spawnSync(
        'gh',
        ['attestation', 'verify', file, '--repo', attester, '--signer-workflow', signerWorkflow, '--source-ref', sourceRef],
        { encoding: 'utf8', stdio: ['ignore', 'ignore', 'pipe'] },
      );
      return { status: out.status ?? 1, stderr: out.stderr ?? '' };
    },
    removeDir: (dir) => rmSync(dir, { recursive: true, force: true }),
    confirm: async (question) => {
      if (!process.stdin.isTTY) return false;
      const rl = createInterface({ input: process.stdin, output: process.stdout });
      const answer = await rl.question(`${question} [y/N] `);
      rl.close();
      return /^y(es)?$/i.test(answer.trim());
    },
    sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
    log: (m) => console.log(m),
    isTTY: Boolean(process.stdin.isTTY),
  };
}

export async function main(argv, deps) {
  let opts;
  try {
    opts = parseArgs(argv);
    if (opts.help) {
      deps.log(readFileSync(fileURLToPath(import.meta.url), 'utf8').split('\nimport ')[0].replace(/^#!.*\n/, '').replace(/^\/\/ ?/gm, ''));
      return EXIT.OK;
    }
    if (!opts.dryRun && !opts.yes && !deps.isTTY) throw new Refusal('not a terminal: pass --yes to proceed without asking');
    if (opts.source === 'release' && !opts.resumedFrom) return await releaseAndResume(opts, argv, deps);
    if (opts.source === 'checkout' && opts.to && !opts.resumedFrom) return await checkoutAndResume(opts, argv, deps);
    const facts = await gather(opts, deps);
    const plan = decide(facts, opts);
    if (plan.upToDate) {
      deps.log(`==> ${facts.origin} already runs ${facts.deployed.version}: nothing to do`);
      return EXIT.OK;
    }
    printPlan(facts, plan, deps);
    if (opts.dryRun) {
      deps.log('==> Dry run: nothing changed');
      return EXIT.OK;
    }
    if (!opts.yes && !(await deps.confirm(`Update ${facts.origin} to ${facts.target.version}?`))) {
      throw new Refusal('not updated');
    }
    return await execute(facts, plan, opts, deps);
  } catch (error) {
    if (error instanceof Refusal) {
      deps.log(`error: ${error.message}`);
      return error.code;
    }
    throw error;
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = await main(process.argv.slice(2), realDeps(await wranglerConfigReader()));
}
