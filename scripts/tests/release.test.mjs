// UPD1: scripts/release/release.mjs, the pieces the release workflow calls.
//   node --test scripts/tests/
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  checkTag,
  checksums,
  cloudflareBundle,
  imageTags,
  notes,
  ociDigest,
  releaseNotesSection,
  schema,
  syncVersions,
  workspaceVersion,
} from '../release/release.mjs';

const temp = () => mkdtempSync(join(tmpdir(), 'upd1-release-'));
const put = (root, path, text = '') => {
  mkdirSync(dirname(join(root, path)), { recursive: true });
  writeFileSync(join(root, path), text);
};
const git = (root, ...args) => {
  const out = spawnSync('git', ['-C', root, '-c', 'user.name=t', '-c', 'user.email=t@example.invalid', ...args], { encoding: 'utf8' });
  assert.equal(out.status, 0, out.stderr);
  return out.stdout;
};

test('the tag must be the workspace release', () => {
  assert.equal(checkTag('v0.2.0', '0.2.0'), '0.2.0');
  assert.throws(() => checkTag('v0.2.1', '0.2.0'), /does not match/);
  assert.throws(() => checkTag('0.2.0', '0.2.0'), /not a vMAJOR/);
  assert.throws(() => checkTag('v0.2.0-rc1', '0.2.0'), /not a vMAJOR/);
});

test('only a tag whose commit is on main is released', () => {
  const root = temp();
  try {
    git(root, 'init', '-q', '-b', 'main');
    put(root, 'a', '1');
    git(root, 'add', '-A');
    git(root, 'commit', '-qm', 'one');
    git(root, 'tag', 'v0.1.0');
    git(root, 'checkout', '-qb', 'side');
    put(root, 'a', '2');
    git(root, 'commit', '-qam', 'side');
    git(root, 'tag', 'v0.1.1');
    assert.equal(checkTag('v0.1.0', '0.1.0', { root, mainRef: 'main' }), '0.1.0');
    assert.throws(() => checkTag('v0.1.1', '0.1.1', { root, mainRef: 'main' }), /not on main/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  assert.match(workspaceVersion(), /^\d+\.\d+\.\d+$/);
});

test('version sync makes Cargo, web, auth and Cloudflare packages agree', () => {
  const root = temp();
  try {
    put(root, 'Cargo.toml', '[workspace.package]\nversion = "0.2.0"\n');
    for (const dir of ['web', 'auth', 'cloudflare']) {
      put(root, `${dir}/package.json`, `${JSON.stringify({ name: dir, ...(dir === 'cloudflare' ? { private: true } : { version: '0.0.0' }) }, null, 2)}\n`);
      put(
        root,
        `${dir}/package-lock.json`,
        `${JSON.stringify({
          name: dir,
          ...(dir === 'cloudflare' ? {} : { version: '0.0.0' }),
          lockfileVersion: 3,
          packages: {
            '': { name: dir, ...(dir === 'cloudflare' ? {} : { version: '0.0.0' }) },
            'node_modules/example': { version: '9.9.9' },
          },
        }, null, 2)}\n`,
      );
    }

    assert.throws(() => syncVersions(root, undefined, { check: true }), /out of sync/);
    assert.equal(syncVersions(root), '0.2.0');
    for (const dir of ['web', 'auth', 'cloudflare']) {
      const manifest = JSON.parse(readFileSync(join(root, dir, 'package.json'), 'utf8'));
      const lock = JSON.parse(readFileSync(join(root, dir, 'package-lock.json'), 'utf8'));
      assert.equal(manifest.version, '0.2.0', dir);
      assert.equal(lock.version, '0.2.0', dir);
      assert.equal(lock.packages[''].version, '0.2.0', dir);
      assert.equal(lock.packages['node_modules/example'].version, '9.9.9', dir);
    }
    assert.equal(syncVersions(root, undefined, { check: true }), '0.2.0');

    assert.equal(syncVersions(root, 'v0.3.0'), '0.3.0');
    assert.equal(workspaceVersion(root), '0.3.0');
    assert.equal(JSON.parse(readFileSync(join(root, 'web/package.json'), 'utf8')).version, '0.3.0');
    assert.throws(() => syncVersions(root, '0.3'), /X\.Y\.Z/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('a release notes entry is required for the exact version', () => {
  const root = temp();
  try {
    put(root, 'docs/release-notes.md', '# Release notes\n\n## Unreleased\n\n- queued\n');
    assert.match(releaseNotesSection(root, '0.2.0'), /queued/);
    assert.throws(() => releaseNotesSection(root, '0.2.0', { required: true }), /no 0\.2\.0 section/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('image tags: the version always; X.Y and latest only when newest', () => {
  const tags = ['v0.1.0', 'v0.1.1', 'v0.2.0', 'v0.2.1', 'vnext', ''];
  const r = 'ghcr.io/angler-ai/runbook';
  assert.deepEqual(imageTags('v0.2.1', tags), [`${r}:0.2.1`, `${r}:0.2`, `${r}:latest`]);
  // A patch to an older line moves that line's tag, never latest.
  assert.deepEqual(imageTags('v0.1.1', tags), [`${r}:0.1.1`, `${r}:0.1`]);
  assert.deepEqual(imageTags('v0.2.0', tags), [`${r}:0.2.0`]);
  assert.deepEqual(imageTags('v0.3.0', [...tags, 'v0.3.0'], 'localhost/x'), ['localhost/x:0.3.0', 'localhost/x:0.3', 'localhost/x:latest']);
});

test('image-tags counts published releases only, not every v* tag', () => {
  const dir = temp();
  try {
    // v0.9.0 is a tag whose release was never published: latest still moves.
    put(dir, 'published', 'v0.1.0\nv0.2.0\n');
    const out = spawnSync(process.execPath, [join(dirname(fileURLToPath(import.meta.url)), '..', 'release', 'release.mjs'), 'image-tags', 'v0.2.1', 'r', join(dir, 'published')], { encoding: 'utf8' });
    assert.equal(out.status, 0, out.stderr);
    assert.deepEqual(out.stdout.trim().split('\n'), ['r:0.2.1', 'r:0.2', 'r:latest']);
    const missing = spawnSync(process.execPath, [join(dirname(fileURLToPath(import.meta.url)), '..', 'release', 'release.mjs'), 'image-tags', 'v0.2.1', 'r'], { encoding: 'utf8' });
    assert.notEqual(missing.status, 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('checksums are sha256sum -c compatible', () => {
  const dir = temp();
  try {
    put(dir, 'b.tar.gz', 'b');
    put(dir, 'a.txt', 'a');
    const lines = checksums(dir);
    assert.deepEqual(lines.map((l) => l.split('  ')[1]), ['a.txt', 'b.tar.gz']);
    const check = spawnSync('shasum', ['-a', '256', '-c', 'SHA256SUMS'], { cwd: dir, encoding: 'utf8' });
    assert.equal(check.status, 0, check.stdout + check.stderr);
    assert.ok(!readFileSync(join(dir, 'SHA256SUMS'), 'utf8').includes('SHA256SUMS'));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('notes list the schema, new migrations per store, contract migrations and changes', () => {
  const root = temp();
  try {
    git(root, 'init', '-q');
    put(root, 'Cargo.toml', '[workspace.package]\nversion = "0.1.0"\n');
    put(root, 'crates/runbook-store-postgres/migrations/0001_history.sql', 'CREATE TABLE a (x INT);\n');
    put(root, 'cloudflare/migrations/d1/0001_history.sql', 'CREATE TABLE a (x INT);\n');
    put(root, 'cloudflare/migrations/do/0001_execution.sql', 'CREATE TABLE b (x INT);\n');
    put(root, 'docs/release-notes.md', '# Release notes\n\n## 0.1.0\n\n- First release.\n');
    git(root, 'add', '-A');
    git(root, 'commit', '-qm', 'first release');
    git(root, 'tag', 'v0.1.0');
    assert.match(notes('v0.1.0', { root }), /New migrations in this first release[\s\S]*postgres: 0001_history\.sql/);

    put(root, 'Cargo.toml', '[workspace.package]\nversion = "0.2.0"\n');
    put(root, 'crates/runbook-store-postgres/migrations/0002_note.sql', 'ALTER TABLE a ADD COLUMN note TEXT;\n');
    put(root, 'cloudflare/migrations/d1/1001_drop.sql', '-- contract(unused-since 0.1.0): 0.1.0 stopped reading a.x\nALTER TABLE a DROP COLUMN x;\n');
    put(root, 'docs/release-notes.md', '# Release notes\n\n## Unreleased\n\n### Later\n\n- not this\n\n## 0.2.0\n\n### Before you update\n\nRead this.\n\n## 0.1.0\n\n- old\n');
    git(root, 'add', '-A');
    git(root, 'commit', '-qm', 'add a note column');
    git(root, 'tag', 'v0.2.0');
    const text = notes('v0.2.0', { root });
    assert.match(text, /^# The Runbook 0\.2\.0/);
    assert.match(text, /\*\*Schema:\*\* postgres 2 · d1 1001 · execution_do 1 · org_do 0 · webhook_do 0/);
    assert.match(text, /deploy\/update\.sh --to 0\.2\.0/);
    assert.match(text, /npm run update -- --to 0\.2\.0/);
    assert.match(text, /New migrations since v0\.1\.0[\s\S]*- postgres: 0002_note\.sql\n  - d1: 1001_drop\.sql/);
    assert.match(text, /Contract migrations[\s\S]*1001_drop\.sql: 0\.1\.0 stopped reading a\.x \(unused since 0\.1\.0\)/);
    assert.match(text, /### Operator notes\n\n#### Before you update\n\nRead this\./);
    assert.ok(!text.includes('not this') && !text.includes('- old'), 'only this release\'s section of docs/release-notes.md');
    assert.match(text, /Rollback floor:\*\* postgres none, d1 0\.1\.0, execution_do none, org_do none, webhook_do none/);
    assert.match(text, /New values\*\*/);
    assert.match(text, /## Changes since v0\.1\.0\n\n- add a note column\n/);
    assert.ok(!text.includes('first release\n'), 'only changes since the previous tag');
    assert.deepEqual(schema(root), { postgres: 2, d1: 1001, execution_do: 1, org_do: 0, webhook_do: 0 });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('the Cloudflare bundle carries the built Workers, web UI, migrations, templates and update.mjs', () => {
  const root = temp();
  const out = temp();
  const extracted = temp();
  try {
    put(root, 'Cargo.toml', '[workspace.package]\nversion = "0.3.0"\n');
    for (const f of ['build/index.js', 'build/index_bg.wasm', 'build/worker/shim.mjs', 'build/cf1-size.json', 'build/cf1-cold-start.json',
      'wrangler.jsonc.example', 'wrangler.auth.jsonc.example', 'package.json', 'package-lock.json', 'README.md', 'scripts/update.mjs',
      'migrations/d1/0001_history.sql', 'migrations/d1/1014_x.sql', 'migrations/do/0005_x.sql', 'migrations/orgdo/0001_x.sql', 'migrations/webhookdo/0002_x.sql',
      'migrations/do-classes.json']) {
      put(root, `cloudflare/${f}`, f);
    }
    put(root, 'crates/runbook-store-postgres/migrations/0001_history.sql');
    put(root, 'auth/dist/worker.mjs', 'auth');
    put(root, 'auth/dist/server.mjs', 'native only');
    put(root, 'auth/drizzle/sqlite/0000_auth.sql', 'auth tables');
    put(root, 'web/dist/index.html', '<!doctype html>');
    const assets = temp();
    put(assets, 'index.html', '<!doctype html>');
    put(assets, '_headers', '/*\n');
    put(root, 'cloudflare/migrations/d1/1014_x.sql', '-- contract(unused-since 0.2.1): x\nDROP TABLE x;\n');
    const digest = `sha256:${'a'.repeat(64)}`;
    const file = cloudflareBundle(out, { root, imageDigest: digest }).finish(assets);
    rmSync(assets, { recursive: true, force: true });
    assert.equal(file, join(out, 'runbook-cloudflare-0.3.0.tar.gz'));
    assert.equal(spawnSync('tar', ['-xzf', file, '-C', extracted]).status, 0);
    const top = join(extracted, 'runbook-cloudflare-0.3.0');
    const list = (dir) => readdirSync(join(top, dir)).sort();
    assert.deepEqual(list(''), ['RELEASE.json', 'auth', 'cloudflare']);
    assert.deepEqual(list('cloudflare/build'), ['index.js', 'index_bg.wasm', 'worker']);
    assert.deepEqual(list('cloudflare/build-assets'), ['_headers', 'index.html']);
    assert.deepEqual(list('cloudflare/migrations'), ['d1', 'do', 'do-classes.json', 'orgdo', 'webhookdo']);
    assert.deepEqual(list('auth/dist'), ['worker.mjs']);
    assert.deepEqual(list('auth/drizzle/sqlite'), ['0000_auth.sql']);
    assert.ok(list('cloudflare').includes('wrangler.jsonc.example') && list('cloudflare/scripts').includes('update.mjs'));
    const release = JSON.parse(readFileSync(join(top, 'RELEASE.json'), 'utf8'));
    assert.deepEqual(release, {
      version: '0.3.0',
      schema: { postgres: 1, d1: 1014, execution_do: 5, org_do: 1, webhook_do: 2 },
      rollback_floor: { postgres: null, d1: '0.2.1', execution_do: null, org_do: null, webhook_do: null },
      image: 'ghcr.io/angler-ai/runbook:0.3.0',
      image_digest: `sha256:${'a'.repeat(64)}`,
    });
    rmSync(join(root, 'web/dist/index.html'));
    assert.throws(() => cloudflareBundle(out, { root }), /web\/dist\/index\.html is missing/);
  } finally {
    for (const d of [root, out, extracted]) rmSync(d, { recursive: true, force: true });
  }
});

test('the OCI archive\'s image index digest is what the publish job must push', () => {
  const dir = temp();
  try {
    const digest = `sha256:${'b'.repeat(64)}`;
    put(dir, 'layout/index.json', JSON.stringify({ schemaVersion: 2, manifests: [{ mediaType: 'application/vnd.oci.image.index.v1+json', digest }] }));
    put(dir, 'layout/oci-layout', '{"imageLayoutVersion":"1.0.0"}');
    assert.equal(spawnSync('tar', ['-cf', join(dir, 'image.tar'), '-C', join(dir, 'layout'), 'index.json', 'oci-layout']).status, 0);
    assert.equal(ociDigest(join(dir, 'image.tar')), digest);
    put(dir, 'two/index.json', JSON.stringify({ manifests: [{ digest }, { digest }] }));
    assert.equal(spawnSync('tar', ['-cf', join(dir, 'two.tar'), '-C', join(dir, 'two'), 'index.json']).status, 0);
    assert.throws(() => ociDigest(join(dir, 'two.tar')), /exactly one image index/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
