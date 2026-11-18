// Builds the browser compiler (B3, A8) and copies it to src/wasm/runbook.wasm.
//   node scripts/build-wasm.mjs                 build; fail without cargo
//   node scripts/build-wasm.mjs --if-available  skip (exit 0) without cargo
// Steps: cargo build --release -p runbook-web-wasm --target wasm32-unknown-unknown,
// then `wasm-opt -Oz` when it is installed, then copy the module and print its
// raw and gzip sizes against the 1.5 MB gzip budget. The module is gitignored.
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync, rmSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';

const BUDGET_GZIP_BYTES = 1.5 * 1024 * 1024;
const repo = fileURLToPath(new URL('../..', import.meta.url));
const target = fileURLToPath(new URL('../src/wasm/runbook.wasm', import.meta.url));
const ifAvailable = process.argv.includes('--if-available');

/** @param {string} command */
function available(command) {
  const probe = spawnSync(command, ['--version'], { stdio: 'ignore' });
  return !probe.error && probe.status === 0;
}

if (!available('cargo')) {
  const message = 'wasm:build: cargo is not installed; src/wasm/runbook.wasm was not built.';
  if (ifAvailable) {
    console.log(`${message} Skipping (the wasm tests skip too).`);
    process.exit(0);
  }
  console.error(message);
  process.exit(1);
}

// Ask cargo for the artifact path: it honours CARGO_TARGET_DIR and friends.
const build = spawnSync(
  'cargo',
  [
    'build',
    '--release',
    '--locked',
    '-p',
    'runbook-web-wasm',
    '--lib',
    '--target',
    'wasm32-unknown-unknown',
    '--message-format=json-render-diagnostics',
  ],
  { cwd: repo, stdio: ['ignore', 'pipe', 'inherit'], encoding: 'utf8', maxBuffer: 64 << 20 },
);
if (build.error || build.status !== 0) {
  console.error('wasm:build: cargo build failed.');
  process.exit(1);
}
/** @type {string | undefined} */
let built;
for (const line of build.stdout.split('\n')) {
  if (!line.startsWith('{')) continue;
  const message = JSON.parse(line);
  if (message.reason === 'compiler-artifact' && message.target?.name === 'runbook_web_wasm') {
    built = message.filenames.find((/** @type {string} */ f) => f.endsWith('.wasm')) ?? built;
  }
}
if (!built) {
  console.error('wasm:build: cargo reported no runbook_web_wasm.wasm artifact.');
  process.exit(1);
}

mkdirSync(fileURLToPath(new URL('../src/wasm/', import.meta.url)), { recursive: true });
let optimized = 'no (wasm-opt not installed)';
if (available('wasm-opt')) {
  rmSync(target, { force: true });
  const opt = spawnSync('wasm-opt', ['-Oz', built, '-o', target], { stdio: 'inherit' });
  if (opt.error || opt.status !== 0) {
    console.warn('wasm:build: wasm-opt failed; using the unoptimized module.');
  } else {
    optimized = 'yes (wasm-opt -Oz)';
  }
}
if (!optimized.startsWith('yes')) copyFileSync(built, target);

const raw = statSync(target).size;
const gzip = gzipSync(readFileSync(target), { level: 9 }).length;
const mb = (/** @type {number} */ n) => `${(n / (1024 * 1024)).toFixed(2)} MB`;
console.log(`wasm:build: src/wasm/runbook.wasm`);
console.log(`  optimized: ${optimized}`);
console.log(`  raw:       ${raw} bytes (${mb(raw)})`);
console.log(`  gzip -9:   ${gzip} bytes (${mb(gzip)}); budget ${mb(BUDGET_GZIP_BYTES)}`);
if (gzip > BUDGET_GZIP_BYTES) {
  console.warn('wasm:build: OVER the 1.5 MB gzip budget (A8): decide before integration.');
}
