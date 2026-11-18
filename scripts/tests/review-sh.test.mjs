// UI-1 review: scripts/dev/review.sh deletes only a state directory it
// created (marked by its marker file), and signals only the processes it
// started (pid and start time recorded), never a recycled pid whose command
// line happens to match.
//
// Safety: every run points REVIEW_PG_ADMIN_URL at a closed port, so no
// database is touched, and uses its own temporary state directory.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFile, spawn, spawnSync } from 'node:child_process';
import { promisify } from 'node:util';
import { existsSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const script = join(dirname(fileURLToPath(import.meta.url)), '..', 'dev', 'review.sh');
const MARKER = '.runbook-review-ui';
const MARKER_TEXT = 'runbook-review-ui state directory (scripts/dev/review.sh)\n';

function review(stateDir, ...args) {
  return spawnSync('bash', [script, ...args], {
    encoding: 'utf8',
    env: {
      ...process.env,
      REVIEW_STATE_DIR: stateDir,
      REVIEW_PG_ADMIN_URL: 'postgres://nobody@127.0.0.1:1/nothing',
      REVIEW_API_PORT: '1',
      REVIEW_SKIP_BUILD: '1',
      PGCONNECT_TIMEOUT: '1',
    },
  });
}

// Async, so this process keeps reaping its own children (a bystander that
// exits must not linger as a zombie that `kill -0` still finds).
async function reviewAsync(stateDir, ...args) {
  try {
    const { stdout, stderr } = await promisify(execFile)('bash', [script, ...args], {
      encoding: 'utf8',
      env: {
        ...process.env,
        REVIEW_STATE_DIR: stateDir,
        REVIEW_PG_ADMIN_URL: 'postgres://nobody@127.0.0.1:1/nothing',
        REVIEW_API_PORT: '1',
        REVIEW_SKIP_BUILD: '1',
        PGCONNECT_TIMEOUT: '1',
      },
    });
    return { status: 0, stdout, stderr };
  } catch (error) {
    return { status: error.code, stdout: error.stdout, stderr: error.stderr };
  }
}

function scratch() {
  const base = mkdtempSync(join(tmpdir(), 'review-sh-test-'));
  return { base, dir: join(base, 'state'), done: () => rmSync(base, { recursive: true, force: true }) };
}

test('stop refuses to delete a directory the script did not create', () => {
  const s = scratch();
  try {
    mkdirSync(s.dir);
    writeFileSync(join(s.dir, 'precious.txt'), 'keep me');
    const r = review(s.dir, 'stop');
    assert.notEqual(r.status, 0, r.stdout + r.stderr);
    assert.match(r.stderr, /not created by this script|refusing/i);
    assert.ok(existsSync(join(s.dir, 'precious.txt')), 'the directory survives');
  } finally {
    s.done();
  }
});

test('a start refuses a directory the script did not create, before touching anything', () => {
  const s = scratch();
  try {
    mkdirSync(s.dir);
    writeFileSync(join(s.dir, 'precious.txt'), 'keep me');
    const r = review(s.dir, '--detach');
    assert.notEqual(r.status, 0, r.stdout + r.stderr);
    assert.match(r.stderr, /not created by this script|refusing/i);
    assert.ok(existsSync(join(s.dir, 'precious.txt')), 'the directory survives');
  } finally {
    s.done();
  }
});

test('stop removes a directory carrying the marker', () => {
  const s = scratch();
  try {
    mkdirSync(s.dir);
    writeFileSync(join(s.dir, MARKER), MARKER_TEXT);
    const r = review(s.dir, 'stop');
    assert.equal(r.status, 0, r.stdout + r.stderr);
    assert.ok(!existsSync(s.dir), 'removed');
  } finally {
    s.done();
  }
});

test('a pid whose command matches but whose start time differs is left alone', async () => {
  const s = scratch();
  // A bystander whose command line contains what the server's does.
  const bystander = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 60000)', 'runbook-server serve'], {
    stdio: 'ignore',
  });
  try {
    await new Promise((resolve) => setTimeout(resolve, 200));
    mkdirSync(s.dir);
    writeFileSync(join(s.dir, MARKER), MARKER_TEXT);
    // A pid file from an older run: the pid alone, or a different start time.
    for (const content of [`${bystander.pid}\n`, `${bystander.pid}\nMon Jan  1 00:00:00 2001\n`]) {
      mkdirSync(s.dir, { recursive: true });
      writeFileSync(join(s.dir, MARKER), MARKER_TEXT);
      writeFileSync(join(s.dir, 'server.pid'), content);
      const r = review(s.dir, 'stop');
      assert.equal(r.status, 0, r.stdout + r.stderr);
      assert.doesNotMatch(r.stdout, /stopped server/);
      assert.equal(bystander.exitCode, null, 'still running');
      assert.doesNotThrow(() => process.kill(bystander.pid, 0));
    }
  } finally {
    bystander.kill('SIGKILL');
    s.done();
  }
});

test('a pid with its recorded start time and command is stopped', async () => {
  const s = scratch();
  const ours = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 60000)', 'runbook-server serve'], {
    stdio: 'ignore',
  });
  const exited = new Promise((resolve) => ours.on('exit', resolve));
  try {
    await new Promise((resolve) => setTimeout(resolve, 200));
    const started = spawnSync('ps', ['-p', String(ours.pid), '-o', 'lstart='], { encoding: 'utf8' }).stdout.trim();
    assert.ok(started);
    mkdirSync(s.dir);
    writeFileSync(join(s.dir, MARKER), MARKER_TEXT);
    writeFileSync(join(s.dir, 'server.pid'), `${ours.pid}\n${started}\n`);
    const r = await reviewAsync(s.dir, 'stop');
    assert.equal(r.status, 0, r.stdout + r.stderr);
    assert.match(r.stdout, /stopped server/);
    await exited;
  } finally {
    ours.kill('SIGKILL');
    s.done();
  }
});
