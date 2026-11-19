#!/usr/bin/env node
// UPD1: the release pieces .github/workflows/release.yml calls (a `v*` tag
// push). Each is also runnable locally; none publishes anything.
//
//   node scripts/release/release.mjs check-tag vX.Y.Z [MAIN_REF]
//       the tag is vMAJOR.MINOR.PATCH, equals Cargo.toml's workspace version,
//       and (with MAIN_REF, e.g. origin/main) its commit is on main
//   node scripts/release/release.mjs image-tags vX.Y.Z REPOSITORY PUBLISHED
//       the image references to push, one per line: X.Y.Z always, X.Y when
//       it is the newest of its line, latest when it is the newest release;
//       newest among the PUBLISHED releases (a file of tags, one per line,
//       e.g. `gh release list`), not every v* tag (re-check #6)
//   node scripts/release/release.mjs floor postgres|cloudflare
//       the rollback floor (the oldest release whose code is safe on this
//       schema), or nothing
//   node scripts/release/release.mjs cloudflare-bundle OUTDIR [IMAGE_DIGEST]
//       runbook-cloudflare-X.Y.Z.tar.gz: the built api Worker (deploy
//       build), the sign-in Worker, the web UI with its _headers, every
//       migration, the config templates and update.mjs (README §0a)
//   node scripts/release/release.mjs notes vX.Y.Z > NOTES.md
//       release notes: schema, upgrade notes (new migrations per store,
//       contract migrations, this release's section of docs/release-notes.md), changes
//   node scripts/release/release.mjs oci-digest IMAGE.tar
//       the image index digest of an OCI archive (docker buildx --output
//       type=oci), which a digest-preserving push keeps
//   node scripts/release/release.mjs checksums DIR
//       DIR/SHA256SUMS for every file in DIR (sha256sum -c format)
//   node scripts/release/release.mjs sync-versions [vX.Y.Z] [--check]
//       make Cargo.toml and the web, auth and Cloudflare package versions
//       (including their lockfile roots) agree; without a version, Cargo wins
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { STORE_DIRS, rollbackFloor } from '../check-migrations.mjs';

/** Each store's rollback floor (null: any release since UPD1). */
export const floors = (root) => Object.fromEntries(Object.keys(STORE_DIRS).map((store) => [store, rollbackFloor(root, store)]));

export const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
export const IMAGE_REPOSITORY = 'ghcr.io/angler-ai/runbook';

const semver = (t) => /^v?(\d+)\.(\d+)\.(\d+)$/.exec(t)?.slice(1).map(Number) ?? null;
const cmp = (a, b) => {
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
  return 0;
};

export function workspaceVersion(root = REPO) {
  const toml = readFileSync(join(root, 'Cargo.toml'), 'utf8');
  return /^version\s*=\s*"([^"]+)"/m.exec(toml.slice(toml.indexOf('[workspace.package]')))?.[1] ?? null;
}

const PACKAGE_DIRS = ['web', 'auth', 'cloudflare'];

function packageVersionEntries(root) {
  const entries = [];
  const readJson = (path) => JSON.parse(readFileSync(join(root, path), 'utf8'));
  const writeJson = (path, value) => writeFileSync(join(root, path), `${JSON.stringify(value, null, 2)}\n`);
  for (const dir of PACKAGE_DIRS) {
    const manifestPath = `${dir}/package.json`;
    if (existsSync(join(root, manifestPath))) {
      entries.push({
        path: manifestPath,
        get() {
          return readJson(manifestPath).version ?? null;
        },
        set(version) {
          const manifest = readJson(manifestPath);
          manifest.version = version;
          writeJson(manifestPath, manifest);
        },
      });
    }
    const lockPath = `${dir}/package-lock.json`;
    if (!existsSync(join(root, lockPath))) continue;
    entries.push({
      path: lockPath,
      get() {
        return readJson(lockPath).version ?? null;
      },
      set(version) {
        const lock = readJson(lockPath);
        lock.version = version;
        writeJson(lockPath, lock);
      },
    });
    entries.push({
      path: `${lockPath} packages[""]`,
      get() {
        return readJson(lockPath).packages?.['']?.version ?? null;
      },
      set(version) {
        const lock = readJson(lockPath);
        lock.packages ??= {};
        lock.packages[''] ??= {};
        lock.packages[''].version = version;
        writeJson(lockPath, lock);
      },
    });
  }
  return entries;
}

/**
 * Make every release version in the checkout agree. `Cargo.toml` is the
 * source when no target is given; an explicit target moves it too. With
 * `check`, nothing is written and any drift is an error. Cargo.lock is
 * deliberately left to `cargo update -w`, as the release steps say.
 */
export function syncVersions(root = REPO, requested, { check = false } = {}) {
  const target = (requested ?? workspaceVersion(root) ?? '').replace(/^v/, '');
  if (!semver(target)) throw new Error(`${requested || target || '(missing)'} is not an X.Y.Z version`);
  const entries = packageVersionEntries(root);
  const mismatches = entries
    .filter((entry) => entry.get() !== target)
    .map((entry) => `${entry.path} has ${entry.get() ?? 'no version'}, want ${target}`);
  const current = workspaceVersion(root);
  if (current !== target) mismatches.unshift(`Cargo.toml has ${current ?? 'no version'}, want ${target}`);
  if (check) {
    if (mismatches.length) throw new Error(`package versions are out of sync:\n${mismatches.join('\n')}`);
    return target;
  }
  if (current !== target) {
    const path = join(root, 'Cargo.toml');
    const toml = readFileSync(path, 'utf8');
    const at = toml.indexOf('[workspace.package]');
    const head = toml.slice(0, at);
    const tail = toml.slice(at).replace(/^version\s*=\s*"[^"]+"/m, `version = "${target}"`);
    writeFileSync(path, head + tail);
  }
  for (const entry of entries) if (entry.get() !== target) entry.set(target);
  return target;
}

export function checkTag(tag, version, { root = REPO, mainRef = null } = {}) {
  if (!/^v\d+\.\d+\.\d+$/.test(tag)) throw new Error(`${tag} is not a vMAJOR.MINOR.PATCH release tag`);
  if (tag.slice(1) !== version) throw new Error(`${tag} does not match Cargo.toml's workspace version ${version}`);
  if (mainRef) {
    // UPD1 review #9: only a commit on main is released.
    const out = spawnSync('git', ['-C', root, 'merge-base', '--is-ancestor', `${tag}^{commit}`, mainRef]);
    if (out.status !== 0) throw new Error(`${tag}'s commit is not on ${mainRef}: only main is released`);
  }
  return version;
}

/** The references to push for `tag`, given every release tag there is. */
export function imageTags(tag, allTags, repository = IMAGE_REPOSITORY) {
  const v = semver(tag);
  if (!v) throw new Error(`${tag} is not a release tag`);
  const releases = allTags.map(semver).filter(Boolean);
  const newest = (list) => list.every((o) => cmp(o, v) <= 0);
  const refs = [`${repository}:${v.join('.')}`];
  if (newest(releases.filter((o) => o[0] === v[0] && o[1] === v[1]))) refs.push(`${repository}:${v[0]}.${v[1]}`);
  if (newest(releases)) refs.push(`${repository}:latest`);
  return refs;
}

const migrationFiles = (root, dir) =>
  existsSync(join(root, dir)) ? readdirSync(join(root, dir)).filter((f) => /^\d{4}_.*\.sql$/.test(f)).sort() : [];
export const STORES = {
  postgres: 'crates/runbook-store-postgres/migrations',
  d1: 'cloudflare/migrations/d1',
  execution_do: 'cloudflare/migrations/do',
  org_do: 'cloudflare/migrations/orgdo',
  webhook_do: 'cloudflare/migrations/webhookdo',
};
/** Each store's schema head (postgres: the migration count, the others their last number). */
export function schema(root = REPO) {
  const out = {};
  for (const [store, dir] of Object.entries(STORES)) {
    const files = migrationFiles(root, dir);
    out[store] = store === 'postgres' ? files.length : Math.max(0, ...files.map((f) => Number(f.slice(0, 4))));
  }
  return out;
}

/** Assemble the Cloudflare edition bundle from built outputs. Returns its path. */
export function cloudflareBundle(outDir, { root = REPO, version = workspaceVersion(root), tar = 'tar', imageDigest = null } = {}) {
  const name = `runbook-cloudflare-${version}`;
  const stage = mkdtempSync(join(tmpdir(), 'runbook-bundle-'));
  const top = join(stage, name);
  const need = (path, hint) => {
    if (!existsSync(join(root, path))) throw new Error(`${path} is missing: ${hint}`);
    return join(root, path);
  };
  const copy = (from, to = from, filter) => cpSync(join(root, from), join(top, to), { recursive: true, filter });
  try {
    need('cloudflare/build/index.js', 'build the api Worker (cd cloudflare && node scripts/build-worker.mjs worker)');
    need('auth/dist/worker.mjs', 'build the sign-in Worker (cd cloudflare && node scripts/build-worker.mjs auth)');
    need('web/dist/index.html', 'build the web UI (cd web && node scripts/build-wasm.mjs && npm run build)');
    copy('cloudflare/build', 'cloudflare/build', (src) => !/cf1-[\w-]+\.json$/.test(src));
    for (const dir of Object.values(STORES).filter((d) => d.startsWith('cloudflare/'))) copy(dir);
    copy('cloudflare/migrations/do-classes.json');
    for (const file of ['wrangler.jsonc.example', 'wrangler.auth.jsonc.example', 'package.json', 'package-lock.json', 'README.md']) {
      copy(`cloudflare/${file}`);
    }
    copy('cloudflare/scripts/update.mjs');
    copy('auth/dist/worker.mjs');
    copy('auth/drizzle/sqlite');
    writeFileSync(
      join(top, 'RELEASE.json'),
      `${JSON.stringify(
        {
          version,
          schema: schema(root),
          // The oldest release whose code is safe on this release's schema,
          // per edition (null: any release since UPD1).
          // Per store (re-check #2): a Durable Object floor is not met by
          // restoring D1.
          rollback_floor: floors(root),
          image: `${IMAGE_REPOSITORY}:${version}`,
          image_digest: imageDigest,
        },
        null,
        2,
      )}\n`,
    );
    return { top, stage, finish: (assets) => finishBundle({ assets, name, stage, outDir, tar, root }) };
  } catch (error) {
    rmSync(stage, { recursive: true, force: true });
    throw error;
  }
}

function finishBundle({ assets, name, stage, outDir, tar }) {
  try {
    if (assets) cpSync(assets, join(stage, name, 'cloudflare', 'build-assets'), { recursive: true });
    mkdirSync(outDir, { recursive: true });
    const file = join(resolve(outDir), `${name}.tar.gz`);
    const out = spawnSync(tar, ['-czf', file, '-C', stage, name], { encoding: 'utf8' });
    if (out.status !== 0) throw new Error(`tar failed: ${out.stderr}`);
    return file;
  } finally {
    rmSync(stage, { recursive: true, force: true });
  }
}

/** The digest of the (single) image index an OCI archive holds. */
export function ociDigest(archive, tar = 'tar') {
  const out = spawnSync(tar, ['-xOf', archive, 'index.json'], { encoding: 'utf8' });
  if (out.status !== 0) throw new Error(`${archive} is not an OCI archive (no index.json)`);
  const manifests = JSON.parse(out.stdout).manifests ?? [];
  if (manifests.length !== 1 || !/^sha256:[0-9a-f]{64}$/.test(manifests[0].digest ?? '')) {
    throw new Error(`${archive} does not hold exactly one image index`);
  }
  return manifests[0].digest;
}

/** SHA256SUMS lines for every regular file in `dir` (sorted, sha256sum format). */
export function checksums(dir) {
  const lines = readdirSync(dir)
    .filter((f) => f !== 'SHA256SUMS' && statSync(join(dir, f)).isFile())
    .sort()
    .map((f) => `${createHash('sha256').update(readFileSync(join(dir, f))).digest('hex')}  ${f}`);
  writeFileSync(join(dir, 'SHA256SUMS'), `${lines.join('\n')}\n`);
  return lines;
}

const git = (root, args) => {
  const out = spawnSync('git', ['-C', root, ...args], { encoding: 'utf8' });
  if (out.status !== 0) throw new Error(`git ${args.join(' ')} failed: ${out.stderr}`);
  return out.stdout;
};

/**
 * The operator-facing notes for `version` from docs/release-notes.md (HOST1's
 * file, newest first): its `## X.Y.Z` section, or `## Unreleased` while the
 * version has none yet. Its `###` headings become `####` under the notes'
 * own headings. Null when there is nothing.
 */
export function releaseNotesSection(root, version, { required = false } = {}) {
  const file = join(root, 'docs', 'release-notes.md');
  if (!existsSync(file)) {
    if (required) throw new Error(`docs/release-notes.md has no ${version} section`);
    return null;
  }
  const text = readFileSync(file, 'utf8');
  const sections = new Map();
  for (const part of text.split(/^## /m).slice(1)) {
    const [heading, ...body] = part.split('\n');
    sections.set(heading.trim().replace(/^v/, ''), body.join('\n').trim());
  }
  const exact = sections.get(version);
  if (required && !exact) throw new Error(`docs/release-notes.md has no ${version} section`);
  const body = exact || sections.get('Unreleased');
  return body ? `### Operator notes\n\n${body.replace(/^### /gm, '#### ')}` : null;
}

/** The release notes for `tag` (Markdown). */
export function notes(tag, { root = REPO } = {}) {
  const v = semver(tag);
  const version = v.join('.');
  const tags = git(root, ['tag', '-l', 'v*']).split('\n').filter((t) => semver(t));
  const previous = tags
    .filter((t) => cmp(semver(t), v) < 0)
    .sort((a, b) => cmp(semver(a), semver(b)))
    .at(-1);
  const heads = schema(root);
  const out = [`# The Runbook ${version}`, ''];
  out.push(
    `**Schema:** ${Object.entries(heads).map(([s, n]) => `${s} ${n}`).join(' · ')} ` +
      '(`/v1/meta` reports it; `runbook-server --version` prints the Postgres one).',
    '',
    '## Upgrade notes',
    '',
    `- **Docker:** \`deploy/update.sh --to ${version}\` (backup, health check, rollback; docs/install.md §11).`,
    `- **Cloudflare:** \`npm run update -- --to ${version}\` in \`cloudflare/\` (README §0a).`,
  );
  const added = {};
  const contracts = [];
  for (const [store, dir] of Object.entries(STORES)) {
    const files = previous
      ? git(root, ['diff', '--name-only', '--diff-filter=A', previous, tag, '--', dir]).split('\n').filter((f) => f.endsWith('.sql'))
      : migrationFiles(root, dir).map((f) => `${dir}/${f}`);
    if (files.length) added[store] = files.map((f) => basename(f));
    for (const f of files) {
      const sql = git(root, ['show', `${tag}:${f}`]);
      for (const m of sql.matchAll(/^--\s*contract\(unused-since\s+(\S+)\)\s*:\s*(.*)$/gim)) {
        contracts.push(`${basename(f)}: ${m[2].trim()} (unused since ${m[1]})`);
      }
    }
  }
  const since = previous ? `since ${previous}` : 'in this first release';
  if (Object.keys(added).length) {
    out.push(`- **New migrations ${since}** (applied by the update):`);
    for (const [store, files] of Object.entries(added)) out.push(`  - ${store}: ${files.join(', ')}`);
  } else {
    out.push(`- **No new migrations ${since}.**`);
  }
  out.push(
    `- **Rollback floor:** ${Object.entries(floors(root)).map(([e, f]) => `${e} ${f ?? 'none'}`).join(', ')} ` +
      '(an install running an older release needs `--restore-backup` to update, since its code is not safe on this schema).',
  );
  if (contracts.length) {
    out.push('- **Contract migrations** (they remove shapes older releases used; rolling back below the release named needs a backup restore):');
    for (const c of contracts) out.push(`  - ${c}`);
  } else if (previous) {
    out.push(`- Every new migration is additive: rolling back to ${previous} keeps working on this schema (expand/contract).`);
  }
  out.push(
    '- **New values** (a status, a kind, an enum member): written only once the previous release reads them; ' +
      'see CONTRIBUTING.md. Anything else the previous release cannot read raises the rollback floor.',
  );
  const handWritten = releaseNotesSection(root, version, { required: true });
  if (handWritten) out.push('', handWritten);
  out.push('', `## Changes ${since}`, '');
  const log = git(root, ['log', '--no-merges', '--format=- %s', previous ? `${previous}..${tag}` : tag]).trim();
  out.push(log || '- (none)');
  out.push(
    '',
    '## Assets',
    '',
    `- Image: \`${IMAGE_REPOSITORY}:${version}\` (linux/amd64, linux/arm64).`,
    `- \`runbook-cli-${version}-{linux,macos}-{x86_64,aarch64}.tar.gz\`, \`runbook-server-${version}-linux-{x86_64,aarch64}.tar.gz\`.`,
    `- \`install.sh\`: installs the CLI for this machine, checked against SHA256SUMS: \`curl -fsSL https://github.com/angler-ai/the-runbook-app/releases/latest/download/install.sh | sh\`.`,
    '- `therunbook-skill.zip`: the therunbook agent skill, to upload to Claude or ChatGPT as a skill: <https://github.com/angler-ai/the-runbook-app/releases/latest/download/therunbook-skill.zip>.',
    `- \`runbook-cloudflare-${version}.tar.gz\`: the Cloudflare edition (built Workers, web UI, migrations, templates).`,
    '- Verify downloads with `sha256sum -c SHA256SUMS`.',
    '',
  );
  return out.join('\n');
}

function main(argv) {
  const [command, ...args] = argv;
  switch (command) {
    case 'sync-versions': {
      const check = args.includes('--check');
      const requested = args.find((arg) => arg !== '--check');
      console.log(syncVersions(REPO, requested, { check }));
      return;
    }
    case 'check-tag':
      console.log(checkTag(args[0] ?? '', workspaceVersion(), { mainRef: args[1] ?? null }));
      return;
    case 'floor':
      console.log(rollbackFloor(REPO, args[0] ?? 'postgres') ?? '');
      return;
    case 'image-tags': {
      if (!args[2]) throw new Error('usage: image-tags vX.Y.Z REPOSITORY PUBLISHED_TAGS_FILE');
      const published = readFileSync(args[2], 'utf8').split('\n').map((t) => t.trim()).filter(Boolean);
      console.log(imageTags(args[0], [...published, args[0]], args[1]).join('\n'));
      return;
    }
    case 'cloudflare-bundle': {
      if (!args[0]) throw new Error('usage: cloudflare-bundle OUTDIR');
      if (args[1] && !/^sha256:[0-9a-f]{64}$/.test(args[1])) throw new Error('IMAGE_DIGEST must be sha256:<64 hex>');
      const bundle = cloudflareBundle(args[0], { imageDigest: args[1] ?? null });
      // The web UI with the _headers the assets engine applies (dev-stack.mjs).
      const assets = mkdtempSync(join(tmpdir(), 'runbook-assets-'));
      return import(join(REPO, 'cloudflare', 'scripts', 'dev-stack.mjs'))
        .then(({ prepareAssets }) => prepareAssets(join(REPO, 'web', 'dist'), join(assets, 'out')))
        .then(() => console.log(bundle.finish(join(assets, 'out'))))
        .finally(() => rmSync(assets, { recursive: true, force: true }));
    }
    case 'notes':
      process.stdout.write(notes(args[0]));
      return;
    case 'oci-digest':
      console.log(ociDigest(args[0]));
      return;
    case 'checksums':
      for (const line of checksums(args[0])) console.log(line);
      return;
    default:
      throw new Error('usage: release.mjs sync-versions|check-tag|image-tags|floor|cloudflare-bundle|notes|oci-digest|checksums ... (see the header)');
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    await main(process.argv.slice(2));
  } catch (error) {
    console.error(`error: ${error.message}`);
    process.exit(1);
  }
}
