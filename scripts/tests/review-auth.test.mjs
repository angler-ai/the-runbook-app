// REVIEW-AUTH: scripts/dev/review.sh email-code sign-in test.
// Starts the review stack with a detached run, requests an OTP code for
// owner@review.local, reads the 6-digit code from the 0600 codes.jsonl file,
// signs in with the code, and verifies that GET /v1/me returns an authenticated session.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const script = join(root, 'scripts', 'dev', 'review.sh');
const targetDir = process.env.CARGO_TARGET_DIR || join(root, '..', 'runbook', 'target');

function getFreePort() {
  return new Promise((resolve, reject) => {
    const s = createServer();
    s.listen(0, '127.0.0.1', () => {
      const port = s.address().port;
      s.close((err) => (err ? reject(err) : resolve(port)));
    });
  });
}

async function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

test('review.sh supports email-code sign-in and captures codes to 0600 file', async (t) => {
  const base = mkdtempSync(join(tmpdir(), 'review-auth-test-'));
  const stateDir = join(base, 'state');
  const dbName = `rb_test_review_auth_${Date.now()}`;
  const apiPort = await getFreePort();
  const vitePort = await getFreePort();

  const env = {
    ...process.env,
    REVIEW_STATE_DIR: stateDir,
    REVIEW_DB_NAME: dbName,
    REVIEW_API_PORT: String(apiPort),
    REVIEW_VITE_PORT: String(vitePort),
    CARGO_TARGET_DIR: existsSync(join(targetDir, 'debug', 'runbook-server')) ? targetDir : undefined,
    REVIEW_SKIP_BUILD: '1',
  };

  t.after(() => {
    try {
      spawnSync('bash', [script, 'stop'], { env, stdio: 'ignore' });
    } finally {
      rmSync(base, { recursive: true, force: true });
    }
  });

  // Start detached review stack
  const start = spawnSync('bash', [script, '--detach'], {
    encoding: 'utf8',
    env,
    maxBuffer: 10 * 1024 * 1024,
  });
  assert.equal(start.status, 0, `start failed:\nSTDOUT:\n${start.stdout}\nSTDERR:\n${start.stderr}`);

  // Check 0600 permissions on codes.jsonl and auth.secret
  const codesFile = join(stateDir, 'codes.jsonl');
  assert.ok(existsSync(codesFile), 'codes.jsonl exists');
  const codesStat = statSync(codesFile);
  assert.equal(codesStat.mode & 0o777, 0o600, 'codes.jsonl must have 0600 permissions');

  const authSecretFile = join(stateDir, 'auth.secret');
  assert.ok(existsSync(authSecretFile), 'auth.secret exists');
  const secretStat = statSync(authSecretFile);
  assert.equal(secretStat.mode & 0o777, 0o600, 'auth.secret must have 0600 permissions');

  const viteUrl = `http://127.0.0.1:${vitePort}`;
  const ownerEmail = 'owner@review.local';

  // Request email OTP sign-in code
  const sendRes = await fetch(`${viteUrl}/api/auth/email-otp/send-verification-otp`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Origin: viteUrl,
    },
    body: JSON.stringify({ email: ownerEmail, type: 'sign-in' }),
  });
  const sendBody = await sendRes.text();
  assert.equal(sendRes.status, 200, `send-verification-otp returned ${sendRes.status}: ${sendBody}`);

  // Read code from codes.jsonl
  let code = null;
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const content = readFileSync(codesFile, 'utf8').trim();
    if (content) {
      const lines = content.split('\n').filter(Boolean);
      const last = JSON.parse(lines[lines.length - 1]);
      if (last.to === ownerEmail && last.code) {
        code = last.code;
        break;
      }
    }
    await sleep(200);
  }
  assert.ok(code, 'expected code to be written to codes.jsonl');
  assert.match(code, /^\d{6}$/, 'code must be a 6-digit number');

  // Verify OTP and sign in
  const verifyRes = await fetch(`${viteUrl}/api/auth/sign-in/email-otp`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Origin: viteUrl,
    },
    body: JSON.stringify({ email: ownerEmail, otp: code }),
  });
  const verifyBody = await verifyRes.text();
  assert.equal(verifyRes.status, 200, `sign-in/email-otp returned ${verifyRes.status}: ${verifyBody}`);

  // Extract session cookie from Set-Cookie headers
  const setCookie = verifyRes.headers.get('set-cookie') || '';
  const match = setCookie.match(/runbook\.session_token=([^;]+)/);
  assert.ok(match, `expected runbook.session_token in set-cookie: ${setCookie}`);
  const cookie = `runbook.session_token=${match[1]}`;

  // Call /v1/me with session cookie
  const meRes = await fetch(`${viteUrl}/v1/me`, {
    headers: {
      Cookie: cookie,
    },
  });
  assert.equal(meRes.status, 200, `/v1/me returned ${meRes.status}`);
  const me = await meRes.json();
  assert.equal(me.credential?.kind, 'session');
  assert.equal(me.user?.name, 'Riley Chen');
  assert.equal(me.org?.role, 'owner');

  // Stop stack
  const stop = spawnSync('bash', [script, 'stop'], {
    encoding: 'utf8',
    env,
  });
  assert.equal(stop.status, 0, `stop failed:\nSTDOUT:\n${stop.stdout}\nSTDERR:\n${stop.stderr}`);
  assert.ok(!existsSync(stateDir), 'state directory must be removed on stop');
});
