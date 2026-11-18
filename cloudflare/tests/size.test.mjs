// CF1 gate: the Worker upload (the wasm module plus its JS glue) stays under
// Cloudflare's Worker size limit, and the spike reports it against the
// plan's 4 MB budget.
//
// CF12: Cloudflare's Workers limits (developers.cloudflare.com/workers/
// platform/limits/, read 2026-11-13) count only the uncompressed bundle,
// 64 MiB on Free and Paid alike; "there is no compressed size limit". The
// 10 MB compressed cap this test used to check is gone, so it checks the
// 64 MiB uncompressed. The 4 MB gzip budget (gzip -9, the closest local
// stand-in for what Cloudflare reports) stays: it keeps cold starts in check.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { gzipSync } from 'node:zlib';
import { join } from 'node:path';
import { assertBuilt, buildDir, scriptPath, wasmPath } from './harness.mjs';

const MB = 1_000_000;
const CAP = 64 * 1_048_576;
const BUDGET = 4 * MB;
// CF5: the size-tuned `[profile.worker]` build (opt-level z, fat LTO, one
// codegen unit, stripped, wasm-opt -Oz) took the upload from 3,757,588 B to
// about 2.46 MB. The Cloudflare edition requires Workers Paid (decision
// 2026-11-13: email sending needs it anyway), so the Free plan's 3 MB limit
// no longer applies.

test('the Worker is under the 64 MiB uncompressed limit and the 4 MB gzip budget', async (t) => {
  assertBuilt();
  const wasm = await readFile(wasmPath);
  const script = await readFile(scriptPath);
  const gzip = (bytes) => gzipSync(bytes, { level: 9 }).length;
  const metrics = {
    wasm_raw_bytes: wasm.length,
    wasm_gzip_bytes: gzip(wasm),
    js_raw_bytes: script.length,
    js_gzip_bytes: gzip(script),
  };
  metrics.total_raw_bytes = metrics.wasm_raw_bytes + metrics.js_raw_bytes;
  metrics.total_gzip_bytes = metrics.wasm_gzip_bytes + metrics.js_gzip_bytes;
  metrics.within_budget = metrics.total_gzip_bytes <= BUDGET;
  t.diagnostic(JSON.stringify(metrics));
  await writeFile(join(buildDir, 'cf1-size.json'), `${JSON.stringify(metrics, null, 2)}\n`);
  assert.ok(metrics.total_raw_bytes < CAP, `${metrics.total_raw_bytes} B exceeds the 64 MiB limit`);
  assert.ok(metrics.within_budget, `gzip ${metrics.total_gzip_bytes} B is over the 4 MB budget`);
});
