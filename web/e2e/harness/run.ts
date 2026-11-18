// `npm run e2e [-- <playwright args>]`: the whole e2e run under the shared
// Postgres lock. It first brings the Rust binaries up to date (`cargo build`,
// stack.ts buildBinaries), then starts the stack (stack.ts), runs Playwright
// against it, and always tears the stack down, including on Ctrl-C.
//
//   npm run e2e                       # full suite
//   npm run e2e -- e2e/sign-in.spec.ts
//   npm run e2e -- --serve            # start the stack and wait (develop tests)
//   npm run e2e:container             # the same specs against the packaged image
//                                     # (`--container`; container.ts)
//   npm run e2e:workers               # flagship and sign-in against the local
//                                     # Cloudflare edition (`--workers`; workers.ts)
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildWorkers } from './workers.ts';
import {
  buildBinaries,
  repoRoot,
  runDir,
  startStack,
  statePath,
  stopActiveStack,
  stopStack,
  webDir,
  type Stack,
} from './stack.ts';

const self = fileURLToPath(import.meta.url);
// `--container` and `--workers` are ours; everything else passes through to
// Playwright.
const container = process.argv.includes('--container');
const workers = process.argv.includes('--workers');
const mode = container ? 'container' : workers ? 'workers' : 'native';
const args = process.argv.slice(2).filter((a) => a !== '--container' && a !== '--workers');

/**
 * Every run holds this checkout's lock (both modes share `e2e/.tokens` and
 * `e2e/.run`). Native mode then also takes the shared Postgres lock:
 * E2E_PG_LOCK, else the nearest `.locks/pg.lock` above web/ (shared by every
 * Postgres suite). Container mode touches no host Postgres and skips it.
 * Always checkout first, then Postgres, so the two never deadlock.
 */
function lockPaths(): string[] {
  mkdirSync(runDir, { recursive: true });
  const checkout = join(runDir, 'harness.lock');
  return container || workers ? [checkout] : [checkout, pgLockPath()];
}

function pgLockPath(): string {
  if (process.env.E2E_PG_LOCK) return process.env.E2E_PG_LOCK;
  for (let dir = webDir; dir !== dirname(dir); dir = dirname(dir)) {
    if (existsSync(join(dir, '.locks'))) return join(dir, '.locks', 'pg.lock');
  }
  throw new Error(
    'No .locks directory above web/: set E2E_PG_LOCK to the shared Postgres lock file.',
  );
}

/** Re-exec this script under lockf (macOS) or flock (Linux), nested per lock, once. */
async function relaunchUnderLock(): Promise<never> {
  const locks = lockPaths();
  const env = { ...process.env, E2E_LOCKED: '1' };
  const command: string[] = [];
  for (const lock of locks) {
    command.push(...(process.platform === 'darwin' ? ['lockf', '-k', lock] : ['flock', lock]));
  }
  command.push(process.execPath, self, ...process.argv.slice(2));
  process.stdout.write(`[e2e] waiting for ${locks.join(' and ')}\n`);
  const child = spawn(command[0] as string, command.slice(1), { stdio: 'inherit', env });
  // The child handles Ctrl-C itself (it shares our process group).
  for (const signal of ['SIGINT', 'SIGTERM'] as const) process.on(signal, () => {});
  const code = await new Promise<number>((r) =>
    child.on('exit', (exitCode, signal) => r(exitCode ?? (signal ? 130 : 1))),
  );
  process.exit(code);
}

/** Browser-side issues (support/fixtures.ts recordBrowserIssues), one file per mode. */
const browserLog = join(runDir, `browser-${mode}.jsonl`);

function runPlaywright(stack: Stack): Promise<number> {
  writeFileSync(browserLog, '');
  const bin = join(webDir, 'node_modules', '@playwright', 'test', 'cli.js');
  const child = spawn(process.execPath, [bin, 'test', ...args], {
    cwd: webDir,
    stdio: 'inherit',
    env: {
      ...process.env,
      E2E_STATE: statePath,
      E2E_BASE_URL: stack.state.baseURL,
      E2E_MODE: mode,
      E2E_BROWSER_LOG: browserLog,
    },
  });
  return new Promise((r) => child.on('exit', (code, signal) => r(code ?? (signal ? 130 : 1))));
}

/**
 * The browser must never refuse a resource under the page's own CSP: a
 * refusal fails the run even when every spec passed, because a spec only
 * notices a missing font, worker or wasm module when it breaks something it
 * asserts on. (Vite's dev server sends no CSP, so this bites in container
 * mode, where the server's real headers apply.) Each test also fails on its
 * own refusals (support/fixtures.ts); this is the backstop, and it fails
 * closed: a missing log or an unreadable line is a failure, not a pass.
 */
function cspRefusals(): string[] {
  if (!existsSync(browserLog)) return [`the browser log ${browserLog} is missing`];
  const found = new Map<string, number>();
  const add = (text: string) => found.set(text, (found.get(text) ?? 0) + 1);
  for (const line of readFileSync(browserLog, 'utf8').split('\n')) {
    if (!line) continue;
    let entry: {
      kind?: string;
      text?: string;
      csp?: boolean;
      directive?: string;
      blocked?: string;
    };
    try {
      entry = JSON.parse(line) as typeof entry;
    } catch {
      add('an unreadable browser-log line');
      continue;
    }
    const consoleText =
      (entry.kind?.startsWith('console.') || entry.kind?.startsWith('worker.console.')) &&
      /Content Security Policy|Content-Security-Policy/i.test(entry.text ?? '');
    if (entry.csp === true || consoleText) {
      add(entry.text ?? `${entry.kind}: ${entry.directive} blocked ${entry.blocked}`);
    }
  }
  return [...found].map(([text, n]) => `${n}× ${text}`);
}

async function main() {
  if (process.env.E2E_LOCKED !== '1') {
    // Up-to-date binaries first, outside the locks (stack.ts buildBinaries).
    try {
      await buildBinaries();
      if (workers) await buildWorkers(repoRoot, webDir, runDir);
    } catch (error) {
      process.stderr.write(`[e2e] ${error instanceof Error ? error.message : String(error)}\n`);
      process.exit(1);
    }
    await relaunchUnderLock();
  }
  const started = Date.now();
  let stack: Stack | null = null;
  let interrupted = false;
  const onSignal = (signal: NodeJS.Signals) => {
    if (interrupted) return;
    interrupted = true;
    process.stderr.write(`[e2e] ${signal}: tearing down\n`);
    // During bring-up `stack` is still null: stop what has started so far.
    const done = stack ? stopStack(stack) : stopActiveStack();
    void done.finally(() => process.exit(130));
  };
  process.on('SIGINT', onSignal);
  process.on('SIGTERM', onSignal);

  let code = 1;
  try {
    stack = await startStack({ container, workers });
    process.stdout.write(
      `[e2e] stack ready in ${((Date.now() - started) / 1000).toFixed(1)} s: web ${stack.state.baseURL}, api ${stack.state.apiURL}\n`,
    );
    if (args[0] === '--serve') {
      process.stdout.write(
        `[e2e] serving; in another shell: E2E_STATE=${statePath} E2E_BASE_URL=${stack.state.baseURL}${mode === 'native' ? '' : ` E2E_MODE=${mode}`} npx playwright test\n[e2e] Ctrl-C stops and drops everything.\n`,
      );
      // A pending promise alone does not keep Node running; in container mode
      // no child process does either, so hold a timer until Ctrl-C.
      await new Promise(() => setInterval(() => {}, 1 << 30));
    }
    code = await runPlaywright(stack);
    const refusals = cspRefusals();
    if (refusals.length > 0) {
      process.stderr.write(
        `[e2e] FAIL: the browser refused resources under the CSP (${browserLog}):\n${refusals.join('\n')}\n`,
      );
      if (code === 0) code = 1;
    }
  } catch (error) {
    process.stderr.write(`[e2e] ${error instanceof Error ? error.message : String(error)}\n`);
    // Whatever failed (bring-up, Playwright or the CSP gate), never exit 0.
    if (code === 0) code = 1;
  } finally {
    if (stack && !interrupted) await stopStack(stack);
  }
  process.stdout.write(
    `[e2e] finished in ${((Date.now() - started) / 1000).toFixed(1)} s (exit ${code})\n`,
  );
  process.exit(code);
}

void main();
