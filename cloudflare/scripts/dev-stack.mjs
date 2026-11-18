// CF7: the local multi-Worker stack: the api Worker (with the web UI as
// static assets), the auth Worker and the mail stub, on one origin under
// one `wrangler dev`, like a deployed site: `/` and client routes, `/v1`,
// `/api/auth`, `/mcp` and `/.well-known`. LOCAL ONLY: no deploy, no remote
// bindings, nothing leaves the machine.
//
//   node scripts/dev-stack.mjs [--port 8787] [--state .wrangler/dev-stack]
//
// The app origin is http://127.0.0.1:<port>; the user-content origin (D50)
// is http://localhost:<port>, a different origin served by the same Worker.
// Secrets are generated once per state directory and written only to its
// `.dev.vars` files (mode 0600); nothing secret is printed.
//
// `startStack` is also used by tests/local-origin.test.mjs and by the web
// e2e harness (web/e2e/harness, `npm run e2e:workers`).
import { spawn, spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync } from 'node:fs';
import { chmod, cp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { experimental_readRawConfig } from 'wrangler';

export const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repo = resolve(root, '..');
const wranglerBin = join(root, 'node_modules', 'wrangler', 'bin', 'wrangler.js');
const CSP_SOURCE = join(repo, 'crates', 'runbook-api', 'src', 'web.rs');

/** The web CSP, read from the Rust constant both editions use. */
export async function contentSecurityPolicy() {
  const source = await readFile(CSP_SOURCE, 'utf8');
  const match = /pub const CONTENT_SECURITY_POLICY: &str = "([^"]+)";/.exec(source);
  if (!match) throw new Error(`no CONTENT_SECURITY_POLICY in ${CSP_SOURCE}`);
  return match[1];
}

/** Copy the built web UI to `out` with the `_headers` the assets engine
 * applies to the files it serves without the Worker (src/web.rs). */
export async function prepareAssets(dist, out) {
  if (!existsSync(join(dist, 'index.html'))) {
    throw new Error(`${dist}/index.html is missing: build the web UI first (cd web && npm run build)`);
  }
  await rm(out, { recursive: true, force: true });
  await cp(dist, out, { recursive: true });
  const csp = await contentSecurityPolicy();
  await writeFile(
    join(out, '_headers'),
    [
      '/*',
      `  Content-Security-Policy: ${csp}`,
      '  Referrer-Policy: no-referrer',
      '  X-Content-Type-Options: nosniff',
      '  Cache-Control: no-cache',
      '/assets/*',
      '  ! Cache-Control',
      '  Cache-Control: public, max-age=31536000, immutable',
      '',
    ].join('\n'),
  );
}

async function raw(name) {
  const { rawConfig } = experimental_readRawConfig({ config: join(root, name) });
  return structuredClone(rawConfig);
}

function dotVars(values) {
  return `${Object.entries(values)
    .map(([k, v]) => `${k}=${JSON.stringify(String(v))}`)
    .join('\n')}\n`;
}

async function secrets(state) {
  const file = join(state, 'secrets.json');
  if (existsSync(file)) return JSON.parse(await readFile(file, 'utf8'));
  const made = {
    internal: randomBytes(32).toString('hex'),
    auth: randomBytes(32).toString('hex'),
    bootstrap: randomBytes(24).toString('hex'),
    upload: randomBytes(32).toString('base64'),
    webhook: randomBytes(32).toString('base64'),
  };
  await writeFile(file, JSON.stringify(made), { mode: 0o600 });
  return made;
}

/** Write the Workers' configs and `.dev.vars` under `state`. With
 * `auth: false` (CF9's contract suite) the api Worker has no AUTH binding
 * and no internal secret, so sign-in is off as on a native server without
 * the auth service. */
export async function writeStack({ state, port, assets, vars = {}, authVars = {}, auth: signIn = true }) {
  const origin = `http://127.0.0.1:${port}`;
  const usercontent = `http://localhost:${port}`;
  await mkdir(state, { recursive: true, mode: 0o700 });
  await chmod(state, 0o700);
  const s = await secrets(state);

  const api = await raw('wrangler.dev.jsonc');
  // The test build (gate fix B1): the local stack's test vars need it.
  api.main = join(root, 'build-test', 'index.js');
  api.assets.directory = assets;
  delete api.$schema;
  if (!signIn) delete api.services;
  const auth = await raw('wrangler.auth.jsonc');
  auth.main = join(repo, 'auth', 'dist', 'worker.mjs');
  auth.d1_databases[0].migrations_dir = join(repo, 'auth', 'drizzle', 'sqlite');
  auth.vars = { ...auth.vars, RUNBOOK_ORIGIN: origin, ...authVars };
  delete auth.$schema;
  const mail = await raw('wrangler.mail.jsonc');
  mail.main = join(root, 'dev', 'mail.mjs');
  delete mail.$schema;

  const files = {};
  for (const [name, config, devVars] of [
    [
      'api',
      api,
      {
        ORIGIN: origin,
        RUNBOOK_USERCONTENT_ORIGIN: usercontent,
        RUNBOOK_UPLOAD_SIGNING_KEY: s.upload,
        ...(signIn ? { RUNBOOK_AUTH_INTERNAL_SECRET: s.internal } : {}),
        BOOTSTRAP_SECRET: s.bootstrap,
        WEBHOOK_SECRET_KEY: s.webhook,
        ...vars,
      },
    ],
    ['auth', auth, { RUNBOOK_AUTH_INTERNAL_SECRET: s.internal, RUNBOOK_AUTH_SECRET: s.auth }],
    ['mail', mail, {}],
  ]) {
    const dir = join(state, name);
    await mkdir(dir, { recursive: true });
    files[name] = join(dir, 'wrangler.json');
    await writeFile(files[name], JSON.stringify(config, null, 2));
    await writeFile(join(dir, '.dev.vars'), dotVars(devVars), { mode: 0o600 });
  }
  return { origin, usercontent, files, bootstrapSecret: s.bootstrap, internalSecret: s.internal };
}

export function freePort() {
  return new Promise((done, fail) => {
    const server = createServer();
    server.once('error', fail);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => done(port));
    });
  });
}

const env = () => ({
  ...process.env,
  WRANGLER_SEND_METRICS: 'false',
  CI: '1',
  NO_UPDATE_NOTIFIER: '1',
  CLOUDFLARE_API_TOKEN: '',
  CLOUDFLARE_ACCOUNT_ID: '',
});

/** Run one wrangler command to completion (local only). */
function wrangler(args, cwd) {
  const out = spawnSync(process.execPath, [wranglerBin, ...args], { cwd, env: env(), encoding: 'utf8' });
  if (out.status !== 0) {
    throw new Error(`wrangler ${args[0]} ${args[1] ?? ''} failed:\n${out.stdout}\n${out.stderr}`);
  }
  return out.stdout;
}

async function waitFor(url, child, ms = 60_000) {
  const deadline = Date.now() + ms;
  for (;;) {
    if (child.exitCode !== null) throw new Error(`wrangler dev exited (${child.exitCode})`);
    try {
      const response = await fetch(url);
      if (response.status < 500) return;
    } catch {}
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${url}`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

/**
 * Start the stack. `assets` is a directory to copy (default web/dist);
 * `state` holds the generated configs, secrets and the local D1/R2/DO data
 * (a fresh temporary directory unless given). Resolves once `/v1/meta`
 * answers; `stop()` ends wrangler and, for a temporary state, deletes it.
 */
export async function startStack({
  port,
  state,
  assets = join(repo, 'web', 'dist'),
  vars = {},
  authVars = {},
  auth = true,
  // Local `wrangler dev` runs no Cron Trigger by itself; with this the
  // caller fires them through `/__scheduled` (CF9's contract suite).
  testScheduled = false,
  log = () => {},
  output,
} = {}) {
  port ??= await freePort();
  const temporary = !state;
  if (!state) {
    const { mkdtemp } = await import('node:fs/promises');
    const { tmpdir } = await import('node:os');
    state = await mkdtemp(join(tmpdir(), 'runbook-dev-stack-'));
  }
  const persist = join(state, 'data');
  const assetsDir = join(state, 'assets');
  await prepareAssets(assets, assetsDir);
  const stack = await writeStack({ state, port, assets: assetsDir, vars, authVars, auth });
  if (auth) {
    log('applying the auth tables to the local D1');
    wrangler(['d1', 'migrations', 'apply', 'DB', '--local', '--persist-to', persist, '-c', stack.files.auth], state);
  }
  const inspector = await freePort();
  log(`starting wrangler dev on ${stack.origin}`);
  const child = spawn(
    process.execPath,
    [
      wranglerBin,
      'dev',
      '-c',
      stack.files.api,
      ...(auth ? ['-c', stack.files.auth, '-c', stack.files.mail] : []),
      '--local',
      '--ip',
      '127.0.0.1',
      '--port',
      String(port),
      '--inspector-port',
      String(inspector),
      '--persist-to',
      persist,
      '--show-interactive-dev-session=false',
      ...(testScheduled ? ['--test-scheduled'] : []),
      '--log-level',
      'log',
    ],
    { cwd: state, env: env(), stdio: ['ignore', 'pipe', 'pipe'] },
  );
  const lines = [];
  const keep = (chunk) => {
    const text = chunk.toString();
    lines.push(text);
    if (lines.length > 400) lines.shift();
    output?.(text);
  };
  child.stdout.on('data', keep);
  child.stderr.on('data', keep);
  const stop = async () => {
    if (child.exitCode === null) {
      child.kill('SIGTERM');
      await new Promise((done) => {
        const timer = setTimeout(() => {
          child.kill('SIGKILL');
          done();
        }, 5_000);
        child.once('exit', () => {
          clearTimeout(timer);
          done();
        });
      });
    }
    if (temporary) await rm(state, { recursive: true, force: true });
  };
  try {
    await waitFor(`${stack.origin}/v1/meta`, child);
  } catch (error) {
    await stop();
    throw new Error(`${error.message}\n${lines.join('')}`);
  }
  return {
    ...stack,
    port,
    state,
    persist,
    child,
    logs: () => lines.join(''),
    /** Run SQL against the stack's local D1 (the e2e seeding shim). */
    d1(sql) {
      return wrangler(
        ['d1', 'execute', 'DB', '--local', '--persist-to', persist, '-c', stack.files.api, '--json', '--command', sql],
        state,
      );
    },
    stop,
  };
}

// `npm run dev`: a persistent stack under cloudflare/.wrangler/dev-stack.
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const arg = (name, fallback) => {
    const i = process.argv.indexOf(name);
    return i > 0 ? process.argv[i + 1] : fallback;
  };
  const stack = await startStack({
    port: Number(arg('--port', '8787')),
    state: resolve(arg('--state', join(root, '.wrangler', 'dev-stack'))),
    log: (m) => console.log(`[dev-stack] ${m}`),
    output: (t) => process.stdout.write(t),
  });
  console.log(`[dev-stack] app ${stack.origin} (user content ${stack.usercontent})`);
  console.log(`[dev-stack] the bootstrap secret is in ${join(stack.state, 'secrets.json')}`);
  const end = async () => {
    await stack.stop();
    process.exit(0);
  };
  process.on('SIGINT', end);
  process.on('SIGTERM', end);
}
