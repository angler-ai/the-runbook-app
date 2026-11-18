// MCP2: the two stacks the OAuth browser test signs in to, each with sign-in
// on and served on a real loopback port, so a real browser drives the
// authorization server's pages:
//   - native: `runbook-server serve` with its auth child (Node), email codes
//     through the `capture` transport, on an owned rb_test_oauth_* Postgres
//     database (needs DATABASE_URL, a built runbook-server and auth/dist);
//   - workers: the api Worker, the auth Worker and the mail stub under
//     Miniflare (cloudflare/tests/harness.mjs startSignInStack; needs the
//     test Worker from `cd cloudflare && npm test` or
//     `node scripts/build-worker.mjs worker-test`).
import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const OWNER = 'owner@example.test';
const repo = fileURLToPath(new URL('../../..', import.meta.url));

export interface OAuthStack {
  origin: string;
  /** The newest sign-in code sent to `email`. */
  code(email: string): Promise<string>;
  stop(): Promise<void>;
}

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      const port = typeof address === 'object' && address ? address.port : 0;
      server.close(() => resolve(port));
    });
  });
}

async function waitFor(url: string, child: ChildProcess | null, what: string): Promise<void> {
  const deadline = Date.now() + 90_000;
  for (;;) {
    if (child && child.exitCode !== null) throw new Error(`${what} exited with ${child.exitCode}`);
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      // not up yet
    }
    if (Date.now() > deadline) throw new Error(`${what} did not come up at ${url}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

async function poll<T>(read: () => Promise<T | undefined> | T | undefined, what: string): Promise<T> {
  const deadline = Date.now() + 15_000;
  for (;;) {
    const found = await read();
    if (found !== undefined) return found;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

export async function startNative(): Promise<OAuthStack> {
  const admin = process.env.DATABASE_URL;
  if (!admin) throw new Error('the native OAuth stack needs DATABASE_URL (a role that may CREATE DATABASE)');
  const target = process.env.CARGO_TARGET_DIR ?? join(repo, 'target');
  const bin = process.env.RUNBOOK_SERVER_BIN ?? join(target, 'debug', 'runbook-server');
  const authBundle = join(repo, 'auth', 'dist', 'server.mjs');
  for (const file of [bin, authBundle]) if (!existsSync(file)) throw new Error(`${file} is missing`);
  // An owned database, created and dropped here only.
  const name = `rb_test_oauth_${process.pid}_${randomBytes(3).toString('hex')}`;
  const made = spawnSync('createdb', [`--maintenance-db=${admin}`, name], { encoding: 'utf8' });
  if (made.status !== 0) throw new Error(`createdb ${name}: ${made.stderr}`);
  const url = new URL(admin);
  url.pathname = `/${name}`;
  const database = url.toString();
  const dir = mkdtempSync(join(tmpdir(), 'runbook-oauth-e2e-'));
  const capture = join(dir, 'codes.jsonl');
  const port = await freePort();
  const origin = `http://127.0.0.1:${port}`;
  const env = {
    PATH: process.env.PATH ?? '',
    HOME: process.env.HOME ?? '',
    DATABASE_URL: database,
    RUNBOOK_ORIGIN: origin,
    RUNBOOK_LISTEN_ADDR: `127.0.0.1:${port}`,
  };
  const dropDatabase = () => {
    spawnSync('dropdb', [`--maintenance-db=${admin}`, '--force', '--if-exists', name]);
    rmSync(dir, { recursive: true, force: true });
  };
  const boot = spawnSync(bin, ['bootstrap', '--owner-email', OWNER, '--owner-name', 'Owner'], { env, encoding: 'utf8' });
  if (boot.status !== 0) {
    dropDatabase();
    throw new Error(`bootstrap: ${boot.stderr}`);
  }
  const server = spawn(bin, ['serve'], {
    env: {
      ...env,
      RUNBOOK_AUTH_SECRET: randomBytes(24).toString('hex'),
      RUNBOOK_AUTH_METHODS: 'email',
      RUNBOOK_AUTH_ALLOWED_DOMAINS: 'example.test',
      RUNBOOK_AUTH_EMAIL_TRANSPORT: 'capture',
      RUNBOOK_AUTH_CAPTURE_FILE: capture,
      RUNBOOK_AUTH_COMMAND: `${process.execPath} ${authBundle}`,
    },
    stdio: ['ignore', 'ignore', 'inherit'],
    detached: true,
  });
  const stop = async () => {
    if (server.pid && server.exitCode === null) {
      try {
        process.kill(-server.pid, 'SIGTERM');
      } catch {
        // already gone
      }
      await new Promise((r) => setTimeout(r, 500));
      try {
        process.kill(-server.pid, 'SIGKILL');
      } catch {
        // already gone
      }
    }
    dropDatabase();
  };
  try {
    await waitFor(`${origin}/api/auth/ok`, server, 'runbook-server and its auth child');
  } catch (error) {
    await stop();
    throw error;
  }
  return {
    origin,
    code: (email) =>
      poll(() => {
        if (!existsSync(capture)) return undefined;
        const lines = readFileSync(capture, 'utf8').split('\n').filter(Boolean);
        const mine = lines.map((l) => JSON.parse(l) as { to: string; code: string }).filter((m) => m.to === email);
        return mine.at(-1)?.code;
      }, `a code for ${email}`),
    stop,
  };
}

interface SignInStack {
  fetch(path: string, init?: RequestInit): Promise<Response>;
  mail(): Promise<{ to: string; code: string }[]>;
  dispose(): Promise<void>;
}

export async function startWorkers(): Promise<OAuthStack> {
  const harness = join(repo, 'cloudflare', 'tests', 'harness.mjs');
  const { startSignInStack } = (await import(harness)) as {
    startSignInStack(options: { vars: Record<string, string>; origin: string; port: number }): Promise<SignInStack>;
  };
  const port = await freePort();
  const origin = `http://127.0.0.1:${port}`;
  const secret = randomBytes(24).toString('hex');
  const stack = await startSignInStack({ vars: { BOOTSTRAP_SECRET: secret }, origin, port });
  const boot = await stack.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': secret, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops', owner_email: OWNER }),
  });
  if (boot.status !== 201) {
    await stack.dispose();
    throw new Error(`bootstrap: ${boot.status} ${await boot.text()}`);
  }
  await waitFor(`${origin}/health`, null, 'the Workers stack');
  return {
    origin,
    code: async (email) => poll(async () => (await stack.mail()).filter((m) => m.to === email).at(-1)?.code, `a code for ${email}`),
    stop: () => stack.dispose(),
  };
}
