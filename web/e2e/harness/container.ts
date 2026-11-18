// Container mode (`npm run e2e:container`, M6): the packaged image under
// deploy/docker-compose.yml instead of a native server plus Vite. The server
// serves the UI and the API on one origin, so the browser, the CLI and the
// specs all use the published loopback port.
//
// Each run is its own compose project, `runbook-e2e-<pid>`, with its own
// volume; `down()` runs `docker compose -p <that project> down -v
// --remove-orphans` and touches no other container, volume or image.
// Compose settings (database passwords, webhook key) are generated per run
// and reach docker only through the environment, never argv or a file.
import { execFile, spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { assertContextHasNoE2ESecrets } from './context-check.ts';

export const PROJECT = /^runbook-e2e-[0-9]+$/;

export interface Container {
  project: string;
  image: string;
  composeFile: string;
  /** Compose interpolation (secrets included): pass to docker only, never print. */
  env: NodeJS.ProcessEnv;
  /** The loopback origin of the server (UI and API). */
  url: string;
  /** psql in the postgres service as its superuser, for the interim first-token seeds. */
  psql: string[];
}

function log(message: string) {
  process.stdout.write(`[e2e] ${message}\n`);
}

function docker(
  args: string[],
  env: NodeJS.ProcessEnv,
  options: { input?: string; log?: string } = {},
): Promise<{ stdout: string; stderr: string }> {
  return new Promise((resolvePromise, reject) => {
    const child = execFile(
      'docker',
      args,
      { env, maxBuffer: 64 * 1024 * 1024 },
      (error, stdout, stderr) => {
        if (error) {
          // docker's own errors carry no secret: they reach it by environment.
          const tail = stderr.split('\n').slice(-20).join('\n');
          reject(
            new Error(`docker ${args.slice(0, 4).join(' ')} failed: ${error.message}\n${tail}`),
          );
        } else {
          resolvePromise({ stdout, stderr });
        }
      },
    );
    if (options.input !== undefined) child.stdin?.end(options.input);
  });
}

export function composeArgs(c: Pick<Container, 'project' | 'composeFile'>, args: string[]) {
  // --env-file /dev/null: an operator's deploy/.env never leaks into a run.
  return ['compose', '-p', c.project, '-f', c.composeFile, '--env-file', '/dev/null', ...args];
}

/** Build the image (docker's layer cache makes an unchanged tree quick) unless reuse is asked for. */
async function ensureImage(image: string, repoRoot: string, runDir: string) {
  const reuse = process.env.E2E_IMAGE_REUSE === '1';
  if (reuse) {
    const exists = await docker(['image', 'inspect', '--format', '{{.Id}}', image], process.env)
      .then(() => true)
      .catch(() => false);
    if (exists) {
      log(`reusing image ${image} (E2E_IMAGE_REUSE=1)`);
      return;
    }
  }
  // U26: never build while .dockerignore would send e2e/.tokens or e2e/.run.
  await assertContextHasNoE2ESecrets(repoRoot);
  const started = Date.now();
  log(`building image ${image} (log: e2e/.run/image-build.log)`);
  const out = createWriteStream(join(runDir, 'image-build.log'), { flags: 'w' });
  const child = spawn('docker', ['build', '--progress', 'plain', '-t', image, repoRoot], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.pipe(out);
  child.stderr.pipe(out);
  const code = await new Promise<number>((r) =>
    child.on('exit', (exitCode, signal) => r(exitCode ?? (signal ? 130 : 1))),
  );
  if (code !== 0) throw new Error(`docker build exited ${code}; see e2e/.run/image-build.log`);
  log(`image ${image} ready in ${((Date.now() - started) / 1000).toFixed(1)} s`);
}

/**
 * Build (or reuse) the image, bring the compose project up on `port`, and
 * wait until both services are healthy. The caller must call `down()` on the
 * returned value (or on the partial value passed in `track`) whatever happens.
 */
export async function upContainer(options: {
  repoRoot: string;
  runDir: string;
  port: number;
  track: (c: Container) => void;
}): Promise<Container> {
  const { repoRoot, runDir, port } = options;
  const project = `runbook-e2e-${process.pid}`;
  if (!PROJECT.test(project)) throw new Error(`refusing compose project ${project}`);
  const composeFile = join(repoRoot, 'deploy', 'docker-compose.yml');
  // setup.sh splits PILOT_PSQL on spaces.
  if (/\s/.test(composeFile)) throw new Error(`the repository path has whitespace: ${composeFile}`);
  const image = process.env.E2E_IMAGE ?? 'runbook-pilot:e2e';
  const url = `http://127.0.0.1:${port}`;

  const env: NodeJS.ProcessEnv = {
    ...process.env,
    RUNBOOK_IMAGE: image,
    POSTGRES_USER: 'runbook',
    POSTGRES_DB: 'runbook',
    RUNBOOK_DB_USER: 'runbook_app',
    POSTGRES_PASSWORD: randomBytes(24).toString('hex'),
    RUNBOOK_DB_PASSWORD: randomBytes(24).toString('hex'),
    RUNBOOK_WEBHOOK_SECRET_KEY: randomBytes(32).toString('base64'),
    RUNBOOK_ORIGIN: url,
    RUNBOOK_BIND_ADDRESS: '127.0.0.1',
    RUNBOOK_HOST_PORT: String(port),
    // As in native mode: the pilot's webhooks point at a closed port.
    RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS: 'true',
    RUNBOOK_ALLOW_LINK_LOCAL_WEBHOOK_TARGETS: 'false',
  };
  delete env.RUNBOOK_TOKEN;
  delete env.RUNBOOK_WEB_DIR; // the image's default: the UI is on
  const container: Container = {
    project,
    image,
    composeFile,
    env,
    url,
    psql: [
      'docker',
      ...composeArgs({ project, composeFile }, ['exec', '-T', 'postgres']),
      'psql',
      '-U',
      'runbook',
      '-d',
      'runbook',
    ],
  };

  await ensureImage(image, repoRoot, runDir);
  options.track(container);
  log(`starting compose project ${project} on ${url}`);
  await docker(
    composeArgs(container, ['up', '-d', '--no-build', '--wait', '--wait-timeout', '120']),
    env,
  );
  return container;
}

/** `runbook-server bootstrap` in the server container; returns its JSON output (holds the owner PAT). */
export async function bootstrapContainer(c: Container): Promise<string> {
  const { stdout } = await docker(
    composeArgs(c, ['exec', '-T', 'server', 'runbook-server', 'bootstrap']),
    c.env,
  );
  return stdout;
}

/**
 * Keeps the server's log, then `down -v --remove-orphans` for this project
 * only. The command runs in its own process group so a second Ctrl-C does
 * not interrupt it.
 */
export async function downContainer(c: Container, runDir: string): Promise<void> {
  if (!PROJECT.test(c.project)) throw new Error(`refusing to tear down ${c.project}`);
  const logs = await docker(composeArgs(c, ['logs', '--no-color', 'server']), c.env).catch(
    () => null,
  );
  if (logs) await writeFile(join(runDir, 'server.log'), logs.stdout + logs.stderr);
  log(`removing compose project ${c.project} (containers, network, volume)`);
  const child = spawn(
    'docker',
    composeArgs(c, ['down', '-v', '--remove-orphans', '--timeout', '5']),
    { env: c.env, stdio: 'ignore', detached: true },
  );
  const code = await new Promise<number>((r) => child.on('exit', (exitCode) => r(exitCode ?? 1)));
  if (code !== 0) throw new Error(`docker compose -p ${c.project} down exited ${code}`);
}
