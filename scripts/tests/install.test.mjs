// install.sh, the one-line CLI installer, against a fake release served
// locally (RUNBOOK_DOWNLOAD_BASE), and the release workflow that ships it.
//   node --test scripts/tests/
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../..', import.meta.url));
const script = join(repo, 'install.sh');
const os = process.platform === 'darwin' ? 'macos' : 'linux';
const arch = process.arch === 'arm64' ? 'aarch64' : 'x86_64';
const version = '9.9.9';
const asset = `runbook-cli-${version}-${os}-${arch}.tar.gz`;

/** A release directory holding a fake `runbook` for this machine and SHA256SUMS. */
function fakeRelease({ tamper = false, listed = true } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'install-sh-'));
  const bin = join(root, 'bin');
  mkdirSync(bin);
  writeFileSync(join(bin, 'runbook'), `#!/bin/sh\necho "runbook ${version}"\n`, { mode: 0o755 });
  const release = join(root, 'release');
  mkdirSync(release);
  const tar = spawnSync('tar', ['-czf', join(release, asset), '-C', bin, 'runbook']);
  assert.equal(tar.status, 0, String(tar.stderr));
  const sum = createHash('sha256').update(readFileSync(join(release, asset))).digest('hex');
  const lines = [`${'0'.repeat(64)}  runbook-server-${version}-linux-x86_64.tar.gz`];
  if (listed) lines.push(`${tamper ? 'f'.repeat(64) : sum}  ${asset}`);
  writeFileSync(join(release, 'SHA256SUMS'), `${lines.sort().join('\n')}\n`);
  return { root, release };
}

async function serve(dir) {
  const server = createServer((req, res) => {
    const path = join(dir, decodeURIComponent(new URL(req.url, 'http://x').pathname));
    if (!path.startsWith(dir) || !existsSync(path)) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200).end(readFileSync(path));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { server, base: `http://127.0.0.1:${server.address().port}` };
}

/** Runs install.sh asynchronously so the in-process server can answer it. */
function run(env) {
  return new Promise((resolve) => {
    const child = spawn('sh', [script], { env: { PATH: process.env.PATH, HOME: env.HOME, ...env } });
    let stderr = '';
    child.stderr.on('data', (chunk) => (stderr += chunk));
    child.on('close', (status) => resolve({ status, stderr }));
  });
}

async function install(options, env = {}) {
  const { root, release } = fakeRelease(options);
  const { server, base } = await serve(release);
  const dir = join(root, 'install');
  try {
    const out = await run({ HOME: root, RUNBOOK_VERSION: version, RUNBOOK_DOWNLOAD_BASE: base, RUNBOOK_INSTALL_DIR: dir, ...env });
    return { ...out, dir, root };
  } finally {
    server.close();
  }
}

test('install.sh installs the build for this machine, checked against SHA256SUMS', async () => {
  const { status, stderr, dir, root } = await install();
  try {
    assert.equal(status, 0, stderr);
    const ran = spawnSync(join(dir, 'runbook'), ['--version'], { encoding: 'utf8' });
    assert.equal(ran.stdout.trim(), `runbook ${version}`);
    assert.match(stderr, new RegExp(`downloading runbook ${version} for ${os}/${arch}`));
    assert.match(stderr, /is not on your PATH/);
    assert.match(stderr, /runbook login/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('install.sh refuses a download whose checksum does not match, and installs nothing', async () => {
  const { status, stderr, dir, root } = await install({ tamper: true });
  try {
    assert.notEqual(status, 0);
    assert.match(stderr, /checksum mismatch/);
    assert.equal(existsSync(join(dir, 'runbook')), false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('install.sh refuses an asset SHA256SUMS does not list', async () => {
  const { status, stderr, root } = await install({ listed: false });
  try {
    assert.notEqual(status, 0);
    assert.match(stderr, /is not in SHA256SUMS/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('install.sh needs a version when given a mirror, and rejects a malformed one', async () => {
  const missing = await install({}, { RUNBOOK_VERSION: '' });
  rmSync(missing.root, { recursive: true, force: true });
  assert.notEqual(missing.status, 0);
  assert.match(missing.stderr, /RUNBOOK_DOWNLOAD_BASE needs RUNBOOK_VERSION/);
  const bad = await install({}, { RUNBOOK_VERSION: '1.0;rm -rf /' });
  rmSync(bad.root, { recursive: true, force: true });
  assert.notEqual(bad.status, 0);
  assert.match(bad.stderr, /bad version/);
});

test('the release builds the macOS CLI and ships install.sh, and SHA256SUMS covers both', () => {
  const workflow = readFileSync(join(repo, '.github/workflows/release.yml'), 'utf8');
  assert.match(workflow, /\n  macos:\n    needs: build\n    runs-on: macos-15\n/);
  assert.match(workflow, /scripts\/release\/macos\.sh "\$VERSION" dist/);
  assert.match(workflow, /\n  publish:\n    needs: \[build, macos\]\n/);
  assert.match(workflow, /install -m 755 install\.sh dist\/install\.sh/);
  // SHA256SUMS is written in publish, after the macOS assets join dist.
  const publish = workflow.slice(workflow.indexOf('\n  publish:'));
  assert.ok(publish.indexOf('name: macos') < publish.indexOf('name: SHA256SUMS'));
  assert.ok(publish.indexOf('name: SHA256SUMS') < publish.indexOf('attest-build-provenance'));
  const macos = readFileSync(join(repo, 'scripts/release/macos.sh'), 'utf8');
  assert.match(macos, /runbook-cli-\$version-macos-\$arch\.tar\.gz/);
  assert.match(macos, /codesign --force --sign -/);
});
