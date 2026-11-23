// Builds the core's redactor for the runner (crates/runbook-redact-wasm,
// R1-F3) and copies it to packages/shared/wasm/runbook_redact.wasm.
//   node scripts/build-wasm.mjs                 build; fail without cargo
//   node scripts/build-wasm.mjs --if-available  skip (exit 0) without cargo
// Same approach as web/scripts/build-wasm.mjs: plain cargo for
// wasm32-unknown-unknown (no wasm-bindgen, no wasm-pack). Cargo honours
// CARGO_TARGET_DIR. The module is gitignored; the image builds its own.
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../..', import.meta.url));
const targetDir = new URL('../packages/shared/wasm/', import.meta.url);
const target = new URL('runbook_redact.wasm', targetDir);

const probe = spawnSync('cargo', ['--version'], { stdio: 'ignore' });
if (probe.error || probe.status !== 0) {
  const message = 'wasm:build: cargo is not installed; the redaction module was not built.';
  if (process.argv.includes('--if-available')) {
    console.log(message);
    process.exit(0);
  }
  console.error(message);
  process.exit(1);
}

const build = spawnSync(
  'cargo',
  [
    'build',
    '--release',
    '--locked',
    '-p',
    'runbook-redact-wasm',
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
let built;
for (const line of build.stdout.split('\n')) {
  if (!line.startsWith('{')) continue;
  const message = JSON.parse(line);
  if (message.reason === 'compiler-artifact' && message.target?.name === 'runbook_redact_wasm') {
    built = message.filenames.find((f) => f.endsWith('.wasm')) ?? built;
  }
}
if (!built) {
  console.error('wasm:build: cargo reported no runbook_redact_wasm.wasm artifact.');
  process.exit(1);
}
mkdirSync(targetDir, { recursive: true });
copyFileSync(built, target);
console.log(`wasm:build: ${fileURLToPath(target)} (${statSync(target).size} bytes)`);
