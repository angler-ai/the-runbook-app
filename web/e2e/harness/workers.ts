// Workers mode (`npm run e2e:workers`, CF7): the same specs against the
// Cloudflare edition run locally: the api Worker with the built web UI as
// its static assets, the auth Worker and a mail stub under one multi-Worker
// `wrangler dev` (cloudflare/scripts/dev-stack.mjs), on local D1, R2 and
// Durable Objects. LOCAL ONLY: no Cloudflare account is used.
//
// The owner comes from the Worker's `/internal/bootstrap` (the operator
// route); people's first tokens go into the local D1 through the PILOT_PSQL
// shim (cloudflare/scripts/d1-psql.mjs), exactly where native mode uses psql.
import { spawn } from 'node:child_process';
import { createWriteStream, existsSync } from 'node:fs';
import { readFile, rm } from 'node:fs/promises';
import { join } from 'node:path';

export interface DevStack {
  origin: string;
  usercontent: string;
  state: string;
  bootstrapSecret: string;
  stop(): Promise<void>;
}

interface DevStackModule {
  startStack(options: {
    state: string;
    assets: string;
    vars: Record<string, string>;
    log: (message: string) => void;
    output: (text: string) => void;
  }): Promise<DevStack>;
}

function step(
  label: string,
  file: string,
  args: string[],
  cwd: string,
  logFile: string,
): Promise<void> {
  const out = createWriteStream(logFile, { flags: 'a' });
  out.write(`\n== ${label}\n`);
  const child = spawn(file, args, { cwd, env: process.env, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stdout?.pipe(out, { end: false });
  child.stderr?.pipe(out, { end: false });
  return new Promise((resolve, reject) =>
    child.on('exit', (code) => {
      out.end();
      if (code === 0) resolve();
      else reject(new Error(`${label} exited ${code}; see ${logFile}`));
    }),
  );
}

/**
 * Build what the Workers stack serves: the browser compiler and the web UI
 * (web/dist), the api Worker and the auth Worker. Cargo runs here, before
 * any lock is taken, as buildBinaries does for native mode.
 */
export async function buildWorkers(repoRoot: string, webDir: string, runDir: string) {
  const logFile = join(runDir, 'workers-build.log');
  const cloudflare = join(repoRoot, 'cloudflare');
  for (const dir of [join(cloudflare, 'node_modules'), join(repoRoot, 'auth', 'node_modules')]) {
    if (!existsSync(dir)) throw new Error(`${dir} is missing: run npm ci there first`);
  }
  await step(
    'wasm:build',
    process.execPath,
    ['scripts/build-wasm.mjs', '--if-available'],
    webDir,
    logFile,
  );
  await step(
    'vite build',
    process.execPath,
    [join(webDir, 'node_modules/vite/bin/vite.js'), 'build'],
    webDir,
    logFile,
  );
  await step(
    'the api Worker',
    process.execPath,
    ['scripts/build-worker.mjs', 'worker-test'],
    cloudflare,
    logFile,
  );
  await step(
    'the auth Worker',
    process.execPath,
    ['scripts/build-worker.mjs', 'auth'],
    cloudflare,
    logFile,
  );
}

/** Start the stack in `runDir/workers` (fresh each run). */
export async function upWorkers(
  repoRoot: string,
  webDir: string,
  runDir: string,
  log: (message: string) => void,
): Promise<DevStack> {
  const state = join(runDir, 'workers');
  await rm(state, { recursive: true, force: true });
  const module = join(repoRoot, 'cloudflare', 'scripts', 'dev-stack.mjs');
  const { startStack } = (await import(module)) as DevStackModule;
  const out = createWriteStream(join(runDir, 'wrangler.log'), { flags: 'w' });
  return startStack({
    state,
    assets: join(webDir, 'dist'),
    // setup.sh points webhooks at a closed loopback port, as natively
    // (RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS there).
    vars: { WEBHOOKS_ALLOW_PRIVATE: 'on' },
    log,
    output: (text) => out.write(text),
  });
}

/**
 * The first organization, owner and PAT through the Worker's operator
 * route (as `runbook-server bootstrap`), with the pilot project.
 */
export async function bootstrapWorkers(stack: DevStack): Promise<{ pat: string }> {
  const response = await fetch(`${stack.origin}/internal/bootstrap`, {
    method: 'POST',
    headers: { 'x-bootstrap-secret': stack.bootstrapSecret, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Pilot', user_name: 'Owner', project_slug: 'pilot' }),
  });
  const text = await response.text();
  if (response.status !== 201) {
    throw new Error(`the Worker bootstrap answered ${response.status}: ${text.slice(0, 300)}`);
  }
  const { personal_token: pat } = JSON.parse(text) as { personal_token: string };
  return { pat };
}

/** The PILOT_PSQL command for this stack (no URL or password in it). */
export function workersPsql(repoRoot: string, stack: DevStack): string[] {
  return ['node', join(repoRoot, 'cloudflare', 'scripts', 'd1-psql.mjs'), stack.state];
}

export async function downWorkers(stack: DevStack) {
  await stack.stop();
  // The state holds the local data and generated secrets: drop it.
  await rm(stack.state, { recursive: true, force: true });
}

export async function wranglerLogTail(runDir: string): Promise<string> {
  const text = await readFile(join(runDir, 'wrangler.log'), 'utf8').catch(() => '');
  return text.split('\n').slice(-40).join('\n');
}
