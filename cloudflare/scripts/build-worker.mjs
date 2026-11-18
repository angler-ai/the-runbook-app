// Build the local Workers with worker-build:
// - crates/runbook-worker into cloudflare/build (the api Worker, CF1/CF2);
// - crates/runbook-worker-conformance into cloudflare/build-conformance (the
//   test-only Worker that runs the store conformance cases in workerd, CF2).
// `node scripts/build-worker.mjs [worker|conformance]` builds one; no
// argument builds both.
//
// Needs worker-build 0.8.7 (the same minor as the pinned `worker` crate):
//   cargo install worker-build --version '=0.8.7' --locked
// esbuild comes from this package's pinned devDependency (ESBUILD_BIN), so
// worker-build does not download one. On first use worker-build downloads
// the wasm-bindgen CLI matching Cargo.lock and, for release builds,
// binaryen's wasm-opt (version 132) into ~/Library/Caches/worker-build
// (Linux: ~/.cache/worker-build). Nothing here talks to a Cloudflare account.
import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const WORKER_BUILD_VERSION = '0.8.7';
const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const TARGETS = {
  // CF5: the api Worker builds with the size-tuned `[profile.worker]`
  // (opt-level z, fat LTO, one codegen unit, stripped) and wasm-opt -Oz.
  // The conformance Worker is a local test harness: plain release, faster.
  // Gate fix B1: `worker` is the deploy build (no test seams); the workerd
  // tests, the dev stack and the contract suite run `worker-test`, the same
  // profile with the `test-seams` feature.
  worker: {
    crate: resolve(root, '../crates/runbook-worker'),
    outDir: join(root, 'build'),
    profile: ['--profile', 'worker'],
  },
  'worker-test': {
    crate: resolve(root, '../crates/runbook-worker'),
    outDir: join(root, 'build-test'),
    profile: ['--profile', 'worker'],
    extra: ['--features', 'test-seams'],
  },
  conformance: {
    crate: resolve(root, '../crates/runbook-worker-conformance'),
    outDir: join(root, 'build-conformance'),
    profile: ['--release'],
  },
};
// CF7: the sign-in Worker (auth/, TypeScript): its own esbuild bundle,
// built here so `npm test` covers the api Worker with it.
function buildAuth() {
  const auth = resolve(root, '../auth');
  if (!existsSync(join(auth, 'node_modules'))) {
    console.error('auth/node_modules is missing: run `npm ci --ignore-scripts` in auth/ first.');
    process.exit(1);
  }
  const result = spawnSync(process.execPath, ['scripts/build.mjs'], { cwd: auth, stdio: 'inherit' });
  if (result.status !== 0) process.exit(result.status ?? 1);
}
if (!process.argv[2]) buildAuth();
if (process.argv[2] === 'auth') {
  buildAuth();
  process.exit(0);
}

const wanted = process.argv[2] ? [process.argv[2]] : Object.keys(TARGETS);
for (const name of wanted) {
  if (!TARGETS[name]) {
    console.error(`unknown build target ${name}; expected one of ${Object.keys(TARGETS).join(', ')}`);
    process.exit(1);
  }
}

function findWorkerBuild() {
  const candidates = [
    process.env.WORKER_BUILD_BIN,
    join(process.env.CARGO_HOME ?? join(homedir(), '.cargo'), 'bin', 'worker-build'),
    'worker-build',
  ].filter(Boolean);
  for (const candidate of candidates) {
    const probe = spawnSync(candidate, ['--version'], { encoding: 'utf8' });
    if (probe.status === 0) return { bin: candidate, version: probe.stdout.trim() };
  }
  return null;
}

const found = findWorkerBuild();
if (!found) {
  console.error(`worker-build not found. Install it with:\n  cargo install worker-build --version '=${WORKER_BUILD_VERSION}' --locked`);
  process.exit(1);
}
if (found.version !== WORKER_BUILD_VERSION) {
  console.error(`worker-build ${found.version} found; CF1 pins ${WORKER_BUILD_VERSION}.`);
  process.exit(1);
}
const esbuild = join(root, 'node_modules', 'esbuild', 'bin', 'esbuild');
if (!existsSync(esbuild)) {
  console.error('esbuild is missing: run `npm ci --ignore-scripts` in cloudflare/ first.');
  process.exit(1);
}

const cargoHome = process.env.CARGO_HOME ?? join(homedir(), '.cargo');
for (const name of wanted) {
  const { crate, outDir, profile, extra = [] } = TARGETS[name];
  const result = spawnSync(found.bin, [...profile, '--out-dir', outDir, crate, ...extra], {
    cwd: crate,
    stdio: 'inherit',
    env: {
      ...process.env,
      PATH: `${join(cargoHome, 'bin')}:${process.env.PATH}`,
      ESBUILD_BIN: esbuild,
      CARGO_INCREMENTAL: process.env.CARGO_INCREMENTAL ?? '0',
    },
  });
  if (result.status !== 0) process.exit(result.status ?? 1);
}
