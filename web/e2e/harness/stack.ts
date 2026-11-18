// The e2e stack (M6): an owned Postgres database, a native runbook-server,
// the pilot org seeded by examples/pilot/setup.sh, and Vite's dev server
// proxying /v1 to it. `startStack` writes `e2e/.tokens/state.json` (IDs
// and paths only); secrets live in 0600 files beside it and are never
// printed. `stopStack` always stops both processes and drops the one
// database it created, `runbook_web_e2e_<pid>`, and nothing else.
//
// Container mode (`{ container: true }`, container.ts) replaces the database,
// the native server and Vite with the packaged image under compose project
// `runbook-e2e-<pid>`; `stopStack` then removes that project and its volume.
import { execFile, spawn, type ChildProcess } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { createWriteStream, existsSync } from 'node:fs';
import { chmod, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { mintToken } from '../support/cli.ts';
import type { E2EState, Person } from '../support/state.ts';
import { bootstrapContainer, downContainer, upContainer, type Container } from './container.ts';
import { bootstrapWorkers, downWorkers, upWorkers, workersPsql, type DevStack } from './workers.ts';

const here = dirname(fileURLToPath(import.meta.url));
export const webDir = resolve(here, '../..');
export const repoRoot = resolve(webDir, '..');
export const tokensDir = join(webDir, 'e2e', '.tokens');
export const runDir = join(webDir, 'e2e', '.run');
export const statePath = join(tokensDir, 'state.json');

const DB_NAME = /^runbook_web_e2e_[0-9]+$/;
const PEOPLE: Person[] = ['alice', 'bob', 'carol'];

export interface Stack {
  state: E2EState;
  server: ChildProcess | null;
  vite: ChildProcess | null;
  dbName: string | null;
  adminUrl: string | null;
  container: Container | null;
  /** Workers mode (CF7): the local multi-Worker `wrangler dev`. */
  workers: DevStack | null;
}

function log(message: string) {
  process.stdout.write(`[e2e] ${message}\n`);
}

/** libpq settings from a postgres:// URL, as environment (never argv). */
export function pgEnv(url: string): Record<string, string> {
  const u = new URL(url);
  if (u.protocol !== 'postgres:' && u.protocol !== 'postgresql:') {
    throw new Error('E2E_PG_ADMIN_URL must be a postgres:// URL');
  }
  const env: Record<string, string> = {};
  if (u.hostname) env.PGHOST = u.hostname;
  if (u.port) env.PGPORT = u.port;
  if (u.username) env.PGUSER = decodeURIComponent(u.username);
  if (u.password) env.PGPASSWORD = decodeURIComponent(u.password);
  const db = decodeURIComponent(u.pathname.replace(/^\//, ''));
  if (db) env.PGDATABASE = db;
  const sslmode = u.searchParams.get('sslmode');
  if (sslmode) env.PGSSLMODE = sslmode;
  return env;
}

function withDatabase(url: string, db: string): string {
  const u = new URL(url);
  u.pathname = `/${db}`;
  return u.toString();
}

function run(
  file: string,
  args: string[],
  options: { env?: NodeJS.ProcessEnv; input?: string; cwd?: string } = {},
): Promise<{ stdout: string; stderr: string }> {
  return new Promise((resolvePromise, reject) => {
    const child = execFile(
      file,
      args,
      { env: options.env ?? process.env, cwd: options.cwd, maxBuffer: 32 * 1024 * 1024 },
      (error, stdout, stderr) => {
        if (error) {
          // stderr is included; none of the commands here print secrets.
          reject(new Error(`${file} ${args[0] ?? ''} failed: ${error.message}\n${stderr}`));
        } else {
          resolvePromise({ stdout, stderr });
        }
      },
    );
    if (options.input !== undefined) child.stdin?.end(options.input);
  });
}

async function psqlAdmin(adminUrl: string, sql: string) {
  await run('psql', ['-X', '-q', '-v', 'ON_ERROR_STOP=1', '-c', sql], {
    env: { ...process.env, ...pgEnv(adminUrl) },
  });
}

function freePort(): Promise<number> {
  return new Promise((resolvePromise, reject) => {
    const srv = createServer();
    srv.unref();
    srv.on('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const address = srv.address();
      const port = typeof address === 'object' && address ? address.port : 0;
      srv.close(() => resolvePromise(port));
    });
  });
}

async function waitForHttp(
  url: string,
  child: ChildProcess | null,
  what: string,
  timeoutMs = 90_000,
) {
  const deadline = Date.now() + timeoutMs;
  let lastError = '';
  while (Date.now() < deadline) {
    if (child && child.exitCode !== null) {
      throw new Error(`${what} exited with code ${child.exitCode}; see ${runDir}`);
    }
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(2_000) });
      if (response.ok) return;
      lastError = `HTTP ${response.status}`;
    } catch (error) {
      lastError = String(error);
    }
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error(`${what} did not answer ${url} within ${timeoutMs} ms (${lastError})`);
}

function startProcess(
  name: string,
  file: string,
  args: string[],
  env: NodeJS.ProcessEnv,
): ChildProcess {
  const out = createWriteStream(join(runDir, `${name}.log`), { flags: 'w' });
  const child = spawn(file, args, { env, cwd: webDir, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stdout?.pipe(out);
  child.stderr?.pipe(out);
  return child;
}

async function stopProcess(child: ChildProcess | null) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise<void>((r) => child.once('exit', () => r()));
  child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), 5_000);
  await exited;
  clearTimeout(timer);
}

async function writeSecret(path: string, secret: string) {
  await writeFile(path, `${secret}\n`, { mode: 0o600, flag: 'wx' });
}

/** The env.state file setup.sh writes: KEY=value lines (IDs only). */
async function readPilotState(pilotDir: string): Promise<Record<string, string>> {
  const text = await readFile(join(pilotDir, 'state.env'), 'utf8');
  const state: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const match = /^([A-Z_]+)=(.*)$/.exec(line);
    if (match?.[1] && match[2] !== undefined) state[match[1]] = match[2];
  }
  return state;
}

function binaries() {
  const target = process.env.CARGO_TARGET_DIR ?? join(repoRoot, 'target');
  const cli = process.env.E2E_RUNBOOK_BIN ?? join(target, 'debug', 'runbook');
  const server = process.env.E2E_SERVER_BIN ?? join(target, 'debug', 'runbook-server');
  return { cli, server };
}

/**
 * Brings the debug binaries up to date before a run, so a stale server can
 * never answer for the code under test: `cargo build -p runbook-server -p
 * runbook-cli --locked`, in container mode too (whose server is the image):
 * building the CLI alone resolves features differently and relinks it, so
 * the two modes would keep rebuilding each other's binary. Incremental, so
 * it is quick when nothing changed.
 * Skipped when E2E_SERVER_BIN or E2E_RUNBOOK_BIN names a binary (it is used
 * as it is) or when cargo is not on PATH (the existing binaries are used,
 * and the log says they may be stale). run.ts calls this before it takes
 * the Postgres lock, so a compile never holds up other suites.
 */
export async function buildBinaries(): Promise<void> {
  if (process.env.E2E_SERVER_BIN || process.env.E2E_RUNBOOK_BIN) {
    log('E2E_SERVER_BIN or E2E_RUNBOOK_BIN is set: using the named binaries without a cargo build');
    return;
  }
  const cargo = process.env.CARGO ?? 'cargo';
  const available = await run(cargo, ['--version']).then(
    () => true,
    () => false,
  );
  if (!available) {
    log('cargo is not on PATH: using the existing binaries, which may be stale');
    return;
  }
  const packages = ['-p', 'runbook-server', '-p', 'runbook-cli'];
  await mkdir(runDir, { recursive: true });
  const logFile = join(runDir, 'cargo-build.log');
  log(`cargo build ${packages.join(' ')} --locked (log: ${logFile})`);
  const started = Date.now();
  const out = createWriteStream(logFile, { flags: 'w' });
  const child = spawn(cargo, ['build', ...packages, '--locked'], {
    cwd: repoRoot,
    env: process.env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout?.pipe(out);
  child.stderr?.pipe(out);
  const code = await new Promise<number>((r) =>
    child.on('exit', (exitCode, signal) => r(exitCode ?? (signal ? 130 : 1))),
  );
  await new Promise<void>((r) => out.end(r));
  if (code !== 0) {
    const tail = (await readFile(logFile, 'utf8')).split('\n').slice(-25).join('\n');
    throw new Error(`cargo build exited ${code}; see ${logFile}\n${tail}`);
  }
  log(`cargo build finished in ${((Date.now() - started) / 1000).toFixed(1)} s`);
}

/** The checkout's commit, and whether tracked files differ from it. */
async function sourceCommit(): Promise<string> {
  try {
    const head = (await run('git', ['-C', repoRoot, 'rev-parse', '--short=12', 'HEAD'])).stdout;
    const dirty = (
      await run('git', ['-C', repoRoot, 'status', '--porcelain', '--untracked-files=no'])
    ).stdout;
    return `${head.trim()}${dirty.trim() ? ' with uncommitted changes' : ''}`;
  } catch {
    return 'unknown (no git)';
  }
}

/** One log line naming the binaries a run uses, when each was built, and the checkout. */
async function logBinaries(bins: string[]) {
  const described = await Promise.all(
    bins.map(async (bin) => {
      const built = await stat(bin).then(
        (s) => s.mtime.toISOString(),
        () => 'missing',
      );
      return `${bin} (built ${built})`;
    }),
  );
  log(`binaries: ${described.join(', ')}; checkout at ${await sourceCommit()}`);
}

/** CLI environment for one person: their own config dir and stored login. */
function cliEnv(pilotDir: string, who: string) {
  const env: NodeJS.ProcessEnv = { ...process.env, XDG_CONFIG_HOME: join(pilotDir, 'config', who) };
  delete env.RUNBOOK_HOST;
  delete env.RUNBOOK_TOKEN;
  return env;
}

/**
 * Insert one first token the way seed-first-token.sh does (secret generated
 * here, only its SHA-256 reaches the database), from `sql`.
 */
async function seedWebToken(options: {
  sql: string;
  file: string;
  orgId: string;
  userId: string;
  projectId: string;
  databaseUrl: string | undefined;
  /** A psql command (container mode), run with `psqlEnv`. */
  psql?: string[];
  psqlEnv?: NodeJS.ProcessEnv;
}) {
  const secret = `trb_pat_${randomBytes(32).toString('base64url')}`;
  const hash = createHash('sha256').update(secret).digest('hex');
  const alphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
  let value = (BigInt(Date.now()) << 80n) | BigInt(`0x${randomBytes(10).toString('hex')}`);
  let ulid = '';
  for (let i = 0; i < 26; i++) {
    ulid = (alphabet[Number(value & 31n)] as string) + ulid;
    value >>= 5n;
  }
  const tokenId = `tok_${ulid}`;
  const psqlArgs = ['-X', '-q', '-t', '-A', '-v', 'ON_ERROR_STOP=1'];
  for (const [k, v] of Object.entries({
    token_id: tokenId,
    org_id: options.orgId,
    user_id: options.userId,
    project_id: options.projectId,
    hash_hex: hash,
  })) {
    psqlArgs.push('-v', `${k}=${v}`);
  }
  let command = 'psql';
  let env: NodeJS.ProcessEnv = process.env;
  if (options.databaseUrl) {
    env = { ...process.env, ...pgEnv(options.databaseUrl) };
  } else if (options.psql) {
    command = options.psql[0] as string;
    psqlArgs.unshift(...options.psql.slice(1));
    env = options.psqlEnv ?? process.env;
  } else {
    const parts = (process.env.PILOT_PSQL ?? '').split(' ').filter(Boolean);
    if (parts.length === 0) throw new Error('an external stack needs PILOT_PSQL to seed tokens');
    command = parts[0] as string;
    psqlArgs.unshift(...parts.slice(1));
  }
  const { stdout } = await run(command, psqlArgs, { env, input: options.sql });
  if (stdout.trim() !== tokenId)
    throw new Error(`the web seed for ${options.userId} was not inserted`);
  await writeSecret(options.file, secret);
}

export const BROWSER_SCOPES = [
  'runbooks:read',
  'executions:read',
  'executions:start',
  'executions:act',
];

/** The editor person (M5): project role `editor`, and a browser token that may author. */
export const EDITOR: Person = 'carol';
export const EDITOR_SCOPES = [
  ...BROWSER_SCOPES,
  'runbooks:write',
  'runbooks:publish',
  'skills:read',
];

/**
 * The pilot person who pauses, resumes and cancels runs from the browser:
 * a project operator (setup.sh makes every pilot person one) whose browser
 * token `<who>.control` adds executions:admin (D27 needs both).
 */
export const CONTROLLER: Person = 'alice';
export const CONTROL_SCOPES = [...BROWSER_SCOPES, 'executions:admin'];

let active: Stack | null = null;

/** How seedWebToken reaches the database outside native mode. */
function psqlOption(stack: Stack): { psql?: string[]; psqlEnv?: NodeJS.ProcessEnv } {
  if (stack.container) return { psql: stack.container.psql, psqlEnv: stack.container.env };
  if (stack.workers) return { psql: workersPsql(repoRoot, stack.workers), psqlEnv: process.env };
  return {};
}

/** Tears down whatever startStack has brought up so far (for Ctrl-C during bring-up). */
export function stopActiveStack(): Promise<void> {
  return active ? stopStack(active) : Promise.resolve();
}

export async function startStack(
  options: { container?: boolean; workers?: boolean } = {},
): Promise<Stack> {
  const stack: Stack = {
    state: undefined as unknown as E2EState,
    server: null,
    vite: null,
    dbName: null,
    adminUrl: null,
    container: null,
    workers: null,
  };
  active = stack;
  const containerMode = options.container === true;
  const workersMode = options.workers === true;
  if ((containerMode || workersMode) && (process.env.E2E_API_URL || process.env.E2E_BASE_URL)) {
    throw new Error(
      `${containerMode ? 'container' : 'workers'} mode picks its own URLs: unset E2E_API_URL and E2E_BASE_URL`,
    );
  }
  await rm(tokensDir, { recursive: true, force: true });
  await mkdir(tokensDir, { recursive: true, mode: 0o700 });
  await chmod(tokensDir, 0o700);
  await mkdir(runDir, { recursive: true });
  const pilotDir = join(tokensDir, 'pilot');
  const { cli, server } = binaries();
  const used = containerMode || workersMode ? [cli] : [server, cli];
  for (const bin of used) {
    if (!existsSync(bin)) {
      throw new Error(
        `${bin} is missing. Build it first: cargo build -p runbook-server -p runbook-cli (or set E2E_RUNBOOK_BIN / E2E_SERVER_BIN).`,
      );
    }
  }
  await logBinaries(used);

  const externalApi = process.env.E2E_API_URL;
  let apiURL: string;
  let databaseUrl: string | undefined;
  let bootstrapFile: string;
  // Container mode: the server's published port is the web origin too.
  const vitePort = process.env.E2E_BASE_URL ? 0 : await freePort();
  let baseURL = process.env.E2E_BASE_URL ?? `http://127.0.0.1:${vitePort}`;

  try {
    if (workersMode) {
      // The Worker serves the web UI itself: one origin, no Vite.
      const workers = await upWorkers(repoRoot, webDir, runDir, log);
      stack.workers = workers;
      apiURL = workers.origin;
      baseURL = workers.origin;
      log("bootstrapping the owner (the Worker's /internal/bootstrap)");
      const boot = await bootstrapWorkers(workers);
      await mkdir(pilotDir, { recursive: true, mode: 0o700 });
      bootstrapFile = join(pilotDir, 'owner.pat');
      await writeSecret(bootstrapFile, boot.pat);
    } else if (containerMode) {
      const container = await upContainer({
        repoRoot,
        runDir,
        port: vitePort,
        track: (c) => {
          stack.container = c;
        },
      });
      apiURL = container.url;
      await waitForHttp(`${apiURL}/v1/meta`, null, 'the container');
      log('bootstrapping the owner (runbook-server bootstrap in the container)');
      const boot = JSON.parse(await bootstrapContainer(container)) as { pat: string };
      await mkdir(pilotDir, { recursive: true, mode: 0o700 });
      bootstrapFile = join(pilotDir, 'owner.pat');
      await writeSecret(bootstrapFile, boot.pat);
    } else if (externalApi) {
      // External stack (for example the compose container): seed it with the
      // operator's bootstrap PAT and PILOT_PSQL, as docs/install.md describes.
      apiURL = externalApi.replace(/\/$/, '');
      const file = process.env.E2E_BOOTSTRAP_TOKEN_FILE;
      if (!file) throw new Error('E2E_API_URL needs E2E_BOOTSTRAP_TOKEN_FILE (and PILOT_PSQL)');
      bootstrapFile = file;
      log(`using the external server at ${apiURL}`);
    } else {
      const adminUrl =
        process.env.E2E_PG_ADMIN_URL ?? 'postgres://runbook_dev@localhost:5432/runbook_dev';
      const dbName = `runbook_web_e2e_${process.pid}`;
      if (!DB_NAME.test(dbName)) throw new Error(`refusing database name ${dbName}`);
      stack.adminUrl = adminUrl;
      log(`creating database ${dbName}`);
      // The name is ours alone (this PID); a leftover from a crashed run
      // with the same PID is dropped first.
      await psqlAdmin(adminUrl, `DROP DATABASE IF EXISTS "${dbName}" WITH (FORCE)`);
      await psqlAdmin(adminUrl, `CREATE DATABASE "${dbName}"`);
      stack.dbName = dbName;
      databaseUrl = withDatabase(adminUrl, dbName);

      const apiPort = await freePort();
      apiURL = `http://127.0.0.1:${apiPort}`;
      const serverEnv: NodeJS.ProcessEnv = {
        ...process.env,
        DATABASE_URL: databaseUrl,
        RUNBOOK_ORIGIN: baseURL.replace(/\/$/, ''),
        RUNBOOK_LISTEN_ADDR: `127.0.0.1:${apiPort}`,
        RUNBOOK_WEBHOOK_SECRET_KEY: randomBytes(32).toString('base64'),
        RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS: 'true',
      };
      delete serverEnv.RUNBOOK_TOKEN;
      log(`starting runbook-server on ${apiURL}`);
      stack.server = startProcess('server', server, ['serve'], serverEnv);
      await waitForHttp(`${apiURL}/v1/meta`, stack.server, 'runbook-server');

      log('bootstrapping the owner');
      const { stdout } = await run(server, ['bootstrap'], { env: serverEnv });
      const boot = JSON.parse(stdout) as { pat: string };
      await mkdir(pilotDir, { recursive: true, mode: 0o700 });
      bootstrapFile = join(pilotDir, 'owner.pat');
      await writeSecret(bootstrapFile, boot.pat);
    }

    // Webhook targets: a closed local port. Deliveries fail and retry in the
    // background, which nothing here depends on.
    const deadPort = await freePort();
    log('seeding the pilot org (examples/pilot/setup.sh)');
    const setupEnv: NodeJS.ProcessEnv = {
      ...process.env,
      RUNBOOK_HOST: apiURL,
      PILOT_DIR: pilotDir,
      RUNBOOK_BOOTSTRAP_TOKEN_FILE: bootstrapFile,
      RUNBOOK_BIN: cli,
      RECEIVER_URL: `http://127.0.0.1:${deadPort}/receiver`,
      RELAY_URL: `http://127.0.0.1:${deadPort}/relay`,
    };
    delete setupEnv.RUNBOOK_TOKEN;
    if (databaseUrl) setupEnv.DATABASE_URL = databaseUrl;
    if (stack.workers) {
      // CF7: the seeds go into the stack's local D1 (d1-psql.mjs).
      setupEnv.PILOT_PSQL = workersPsql(repoRoot, stack.workers).join(' ');
      delete setupEnv.DATABASE_URL;
    }
    if (stack.container) {
      // `docker compose … exec -T postgres psql`: the compose settings it
      // interpolates come from the environment.
      Object.assign(setupEnv, stack.container.env, { PILOT_PSQL: stack.container.psql.join(' ') });
      delete setupEnv.DATABASE_URL;
      delete setupEnv.RUNBOOK_TOKEN;
    }
    const setup = await run('bash', [join(repoRoot, 'examples/pilot/setup.sh')], {
      env: setupEnv,
      cwd: repoRoot,
    });
    await writeFile(join(runDir, 'setup.log'), setup.stdout + setup.stderr);
    const pilot = await readPilotState(pilotDir);
    for (const who of PEOPLE) {
      if (!existsSync(join(pilotDir, `${who}.seed`)) || !existsSync(join(pilotDir, `${who}.pat`))) {
        throw new Error(`setup.sh did not give ${who} a seed and a token; see ${runDir}/setup.log`);
      }
    }

    const required = (key: string) => {
      const value = pilot[key];
      if (!value) throw new Error(`setup.sh state is missing ${key}`);
      return value;
    };

    // The pilot seed (seed-first-token.sql) carries runbooks:read with the
    // executions:* scopes, so a pilot person can mint the narrow browser token
    // the sign-in page recommends. The harness seeds its own first token per
    // person from the same SQL so browser tokens never depend on setup.sh's.
    const webSeedSql = await readFile(
      join(repoRoot, 'examples/pilot/seed-first-token.sql'),
      'utf8',
    );
    if (!webSeedSql.includes("'runbooks:read'")) {
      throw new Error(
        'examples/pilot/seed-first-token.sql lost runbooks:read; the web sign-in needs it',
      );
    }
    log('seeding a web seed (runbooks:read added) per person and minting browser tokens');
    const whoami = await run(cli, ['whoami', '--json'], { env: cliEnv(pilotDir, 'owner') });
    const orgId = (JSON.parse(whoami.stdout) as { organizations: { id: string }[] })
      .organizations[0]?.id;
    if (!orgId) throw new Error('the owner has no organization');
    for (const who of PEOPLE) {
      const userId = required(`${who.toUpperCase()}_ID`);
      await seedWebToken({
        sql: webSeedSql,
        file: join(pilotDir, `${who}.seed-web`),
        orgId,
        userId,
        projectId: required('PROJECT_ID'),
        databaseUrl,
        ...psqlOption(stack),
      });
      await mintToken(
        { cli, pilotDir, apiURL, project: 'pilot' },
        who,
        join(tokensDir, `${who}.browser`),
        {
          name: `${who}-browser-e2e`,
          scopes: BROWSER_SCOPES,
          seed: 'seed-web',
        },
      );
    }
    // The editor (M5): an `editor` project role, a seed that carries the
    // authoring scopes (a token never exceeds its caller), and from it a
    // browser token `<who>.editor` with them. The seed SQL is the pilot's,
    // with only its scope list widened.
    log(`making ${EDITOR} a project editor with an authoring browser token`);
    await run(
      cli,
      [
        'project',
        'member',
        'add',
        required('PROJECT_ID'),
        required(`${EDITOR.toUpperCase()}_ID`),
        '--role',
        'editor',
      ],
      { env: cliEnv(pilotDir, 'owner') },
    );
    const editorSeedSql = webSeedSql.replace(
      "'runbooks:read',",
      "'runbooks:read', 'runbooks:write', 'runbooks:publish', 'skills:read',",
    );
    if (editorSeedSql === webSeedSql) throw new Error('could not widen the seed for the editor');
    await seedWebToken({
      sql: editorSeedSql,
      file: join(pilotDir, `${EDITOR}.seed-editor`),
      orgId,
      userId: required(`${EDITOR.toUpperCase()}_ID`),
      projectId: required('PROJECT_ID'),
      databaseUrl,
      ...psqlOption(stack),
    });
    await mintToken(
      { cli, pilotDir, apiURL, project: 'pilot' },
      EDITOR,
      join(tokensDir, `${EDITOR}.editor`),
      { name: `${EDITOR}-editor-e2e`, scopes: EDITOR_SCOPES, seed: 'seed-editor' },
    );

    // The controller: a seed that also carries executions:admin (a token
    // never exceeds its caller), and from it the browser token
    // `<who>.control`. The seed SQL is the pilot's, with only its scopes widened.
    log(`giving ${CONTROLLER} a browser token with executions:admin`);
    const controlSeedSql = webSeedSql.replace(
      "'runbooks:read',",
      "'runbooks:read', 'executions:admin',",
    );
    if (controlSeedSql === webSeedSql) throw new Error('could not widen the seed for control');
    await seedWebToken({
      sql: controlSeedSql,
      file: join(pilotDir, `${CONTROLLER}.seed-control`),
      orgId,
      userId: required(`${CONTROLLER.toUpperCase()}_ID`),
      projectId: required('PROJECT_ID'),
      databaseUrl,
      ...psqlOption(stack),
    });
    await mintToken(
      { cli, pilotDir, apiURL, project: 'pilot' },
      CONTROLLER,
      join(tokensDir, `${CONTROLLER}.control`),
      { name: `${CONTROLLER}-control-e2e`, scopes: CONTROL_SCOPES, seed: 'seed-control' },
    );

    stack.state = {
      baseURL,
      apiURL,
      cli,
      pilotDir,
      tokensDir,
      project: 'pilot',
      projectId: required('PROJECT_ID'),
      users: { alice: required('ALICE_ID'), bob: required('BOB_ID'), carol: required('CAROL_ID') },
      teams: { oncall: required('ONCALL_TEAM_ID'), engLeads: required('ENG_LEADS_TEAM_ID') },
      editor: EDITOR,
      controller: CONTROLLER,
    };
    await writeFile(statePath, JSON.stringify(stack.state, null, 2), { mode: 0o600 });

    if (!process.env.E2E_BASE_URL && !containerMode && !workersMode) {
      log(`starting Vite on ${baseURL}`);
      const viteEnv: NodeJS.ProcessEnv = {
        ...process.env,
        RUNBOOK_DEV_API: apiURL,
        BROWSER: 'none',
      };
      delete viteEnv.RUNBOOK_TOKEN;
      stack.vite = startProcess(
        'vite',
        process.execPath,
        [
          join(webDir, 'node_modules/vite/bin/vite.js'),
          '--port',
          String(vitePort),
          '--strictPort',
          '--host',
          '127.0.0.1',
        ],
        viteEnv,
      );
      await waitForHttp(`${baseURL}/`, stack.vite, 'vite');
    }
    await waitForHttp(`${baseURL}/v1/meta`, stack.vite, 'the web origin /v1 proxy');
    return stack;
  } catch (error) {
    await stopStack(stack);
    throw error;
  }
}

let stopping: Promise<void> | null = null;

/**
 * Idempotent: stops Vite and the server (or removes the compose project),
 * drops the owned database, deletes secrets.
 */
export function stopStack(stack: Stack): Promise<void> {
  stopping ??= (async () => {
    const problems: string[] = [];
    await stopProcess(stack.vite).catch((e: unknown) => problems.push(String(e)));
    await stopProcess(stack.server).catch((e: unknown) => problems.push(String(e)));
    if (stack.container) {
      await downContainer(stack.container, runDir).catch((e: unknown) => problems.push(String(e)));
    }
    if (stack.workers) {
      await downWorkers(stack.workers).catch((e: unknown) => problems.push(String(e)));
    }
    if (stack.dbName && stack.adminUrl) {
      if (!DB_NAME.test(stack.dbName)) {
        problems.push(`refusing to drop ${stack.dbName}`);
      } else {
        log(`dropping database ${stack.dbName}`);
        await psqlAdmin(
          stack.adminUrl,
          `DROP DATABASE IF EXISTS "${stack.dbName}" WITH (FORCE)`,
        ).catch((e: unknown) => problems.push(String(e)));
      }
    }
    if (process.env.E2E_KEEP_TOKENS !== '1') {
      await rm(tokensDir, { recursive: true, force: true }).catch((e: unknown) =>
        problems.push(String(e)),
      );
    }
    if (problems.length > 0)
      process.stderr.write(`[e2e] teardown problems:\n${problems.join('\n')}\n`);
  })();
  return stopping;
}
