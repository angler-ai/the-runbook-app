// CF9: the two builds the contract suite drives, both the open source
// edition with the same configuration where it matters (sign-in off,
// uploads on a separate user-content origin, private webhook targets
// allowed so a local receiver can be named):
// - native: `runbook-server serve` on its own `rb_test_contract_*` Postgres
//   database (created and dropped here; run under the pg lock), owner from
//   `runbook-server bootstrap`;
// - workers: the local `wrangler dev` stack (scripts/dev-stack.mjs) with no
//   auth Worker, owner from the Worker's `/internal/bootstrap`.
// Both bootstrap the same names, so they start from the same state. LOCAL
// ONLY.
import { execFile, spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync } from 'node:fs';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { freePort, root, startStack } from '../scripts/dev-stack.mjs';

const repo = resolve(root, '..');
export const OWNER = { organization: 'Runbook', user_name: 'Administrator', project_slug: 'default' };

function run(file, args, env) {
  return new Promise((done, fail) => {
    execFile(file, args, { env, maxBuffer: 16 * 1024 * 1024 }, (error, stdout, stderr) =>
      error ? fail(new Error(`${file} ${args[0]} failed: ${error.message}\n${stderr}`)) : done(stdout),
    );
  });
}

async function waitFor(url, child, what) {
  const deadline = Date.now() + 90_000;
  for (;;) {
    if (child && child.exitCode !== null) throw new Error(`${what} exited (${child.exitCode})`);
    try {
      if ((await fetch(url)).status < 500) return;
    } catch {}
    if (Date.now() > deadline) throw new Error(`${what} did not answer ${url}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

/** libpq settings from a postgres:// URL, as environment (never argv). */
function pgEnv(url) {
  const u = new URL(url);
  const env = {};
  if (u.hostname) env.PGHOST = u.hostname;
  if (u.port) env.PGPORT = u.port;
  if (u.username) env.PGUSER = decodeURIComponent(u.username);
  if (u.password) env.PGPASSWORD = decodeURIComponent(u.password);
  const db = decodeURIComponent(u.pathname.replace(/^\//, ''));
  if (db) env.PGDATABASE = db;
  return env;
}

/** A small web UI fixture: the contract is the API's, not the UI's. */
async function webFixture() {
  const dir = await mkdtemp(join(tmpdir(), 'runbook-contract-web-'));
  await mkdir(join(dir, 'assets'));
  await writeFile(join(dir, 'index.html'), '<!doctype html><title>contract</title>');
  return dir;
}

export async function startNative({ databaseUrl, log = () => {} }) {
  if (!databaseUrl) throw new Error('the native target needs DATABASE_URL');
  const target = process.env.CARGO_TARGET_DIR ?? join(repo, 'target');
  const bin = process.env.RUNBOOK_SERVER_BIN ?? join(target, 'debug', 'runbook-server');
  if (!existsSync(bin)) throw new Error(`${bin} is missing: cargo build -p runbook-server`);
  const name = `rb_test_contract_${process.pid}_${randomBytes(3).toString('hex')}`;
  const admin = { ...process.env, ...pgEnv(databaseUrl) };
  log(`creating database ${name}`);
  await run('psql', ['-X', '-q', '-v', 'ON_ERROR_STOP=1', '-c', `CREATE DATABASE "${name}"`], admin);
  const dbUrl = new URL(databaseUrl);
  dbUrl.pathname = `/${name}`;
  const [port, contentPort] = [await freePort(), await freePort()];
  const origin = `http://127.0.0.1:${port}`;
  const usercontent = `http://localhost:${contentPort}`;
  const env = {
    ...process.env,
    DATABASE_URL: dbUrl.toString(),
    RUNBOOK_LISTEN_ADDR: `127.0.0.1:${port}`,
    RUNBOOK_ORIGIN: origin,
    RUNBOOK_WEBHOOK_SECRET_KEY: randomBytes(32).toString('base64'),
    RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS: 'true',
    // The api Worker's request-body cap on every route (CF12: native's JSON
    // cap, 2 MiB + 64 KiB). Native allows skill uploads up to this variable
    // (64 MiB by default), a configuration difference rather than a contract
    // one; with it set to the Worker's cap, discovery and every route agree.
    RUNBOOK_MAX_REQUEST_BYTES: '2162688',
    RUNBOOK_UPLOAD_SIGNING_KEY: randomBytes(32).toString('base64'),
    RUNBOOK_USERCONTENT_ORIGIN: usercontent,
    RUNBOOK_USERCONTENT_LISTEN_ADDR: `127.0.0.1:${contentPort}`,
  };
  for (const key of Object.keys(env)) {
    if (/^RUNBOOK_AUTH_/.test(key) || key === 'RUNBOOK_TOKEN') delete env[key];
  }
  const drop = async () => {
    await run('psql', ['-X', '-q', '-c', `DROP DATABASE IF EXISTS "${name}" WITH (FORCE)`], admin).catch(() => {});
  };
  let child;
  try {
    const boot = JSON.parse(await run(bin, ['bootstrap'], env));
    child = spawn(bin, ['serve'], { env, stdio: ['ignore', 'pipe', 'pipe'] });
    const lines = [];
    child.stdout.on('data', (c) => lines.push(c.toString()));
    child.stderr.on('data', (c) => lines.push(c.toString()));
    await waitFor(`${origin}/v1/meta`, child, 'runbook-server');
    return {
      name: 'native',
      origin,
      usercontent,
      pat: boot.pat,
      service: boot.service,
      org: boot.org_id,
      user: boot.user_id,
      project: boot.project_id,
      logs: () => lines.join(''),
      async stop() {
        if (child.exitCode === null) {
          child.kill('SIGTERM');
          await new Promise((r) => child.once('exit', r));
        }
        await drop();
      },
    };
  } catch (error) {
    child?.kill('SIGKILL');
    await drop();
    throw error;
  }
}

export async function startWorkers({ log = () => {} } = {}) {
  const assets = await webFixture();
  const stack = await startStack({
    assets,
    auth: false,
    testScheduled: true,
    vars: { WEBHOOKS_ALLOW_PRIVATE: 'on' },
    log,
  });
  try {
    const response = await fetch(`${stack.origin}/internal/bootstrap`, {
      method: 'POST',
      headers: { 'x-bootstrap-secret': stack.bootstrapSecret, 'content-type': 'application/json' },
      body: JSON.stringify(OWNER),
    });
    const text = await response.text();
    if (response.status !== 201) throw new Error(`bootstrap answered ${response.status}: ${text}`);
    const boot = JSON.parse(text);
    return {
      name: 'workers',
      origin: stack.origin,
      usercontent: stack.usercontent,
      pat: boot.personal_token,
      service: boot.service_token,
      org: boot.org_id,
      user: boot.user_id,
      project: boot.project_id,
      logs: () => stack.logs(),
      /** Fire the minute Cron (webhook redelivery, settles), which the
       * native server's workers do continuously. */
      async tick() {
        await fetch(`${stack.origin}/__scheduled?cron=${encodeURIComponent('* * * * *')}`).catch(() => {});
      },
      async stop() {
        await stack.stop();
        await rm(assets, { recursive: true, force: true });
      },
    };
  } catch (error) {
    await stack.stop();
    await rm(assets, { recursive: true, force: true });
    throw error;
  }
}
