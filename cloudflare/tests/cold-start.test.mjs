// CF1: local cold start, measured as well as a laptop can. These numbers
// are indicative only: production Workers pre-warm isolates during the TLS
// handshake and run on different hardware. Each round boots a fresh workerd
// (Miniflare) and times:
// - boot: `new Miniflare()` until ready (workerd start, script and wasm
//   compile);
// - first_fetch: the first Worker request (isolate start, glue evaluation,
//   wasm instantiation);
// - first_do: the first request to a new ExecutionDO that runs the full
//   start commit (object construction, schema, compile the runbook, settle,
//   one SQLite transaction, alarm);
// - warm_fetch / warm_do: medians of later requests.
// Node's own WebAssembly.compile of the module is reported as a proxy for
// V8's compile cost.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { buildDir, executionId, startWorker, wasmPath } from './harness.mjs';

const ROUNDS = 3;
const WARM = 20;
const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
};
const round = (ms) => Math.round(ms * 10) / 10;

async function timed(action) {
  const start = performance.now();
  const result = await action();
  return [performance.now() - start, result];
}

test('local cold start is measured and recorded', async (t) => {
  const rounds = [];
  for (let i = 0; i < ROUNDS; i += 1) {
    const [boot, worker] = await timed(() => startWorker());
    try {
      const [firstFetch, health] = await timed(() => worker.fetch('/health'));
      assert.equal(health.status, 200);
      await health.text();
      const id = executionId();
      const [firstDo, started] = await timed(() => worker.object(id, '/start', { method: 'POST' }));
      assert.equal(started.status, 200);
      const warmFetch = [];
      const warmDo = [];
      for (let j = 0; j < WARM; j += 1) {
        const [ms, response] = await timed(() => worker.fetch('/health'));
        await response.text();
        warmFetch.push(ms);
        const [doMs, state] = await timed(() => worker.object(id, '/state'));
        assert.equal(state.status, 200);
        warmDo.push(doMs);
      }
      rounds.push({ boot, firstFetch, firstDo, warmFetch: median(warmFetch), warmDo: median(warmDo) });
    } finally {
      await worker.dispose();
    }
  }
  const wasm = await readFile(wasmPath);
  const compiles = [];
  for (let i = 0; i < ROUNDS; i += 1) {
    const [ms] = await timed(() => WebAssembly.compile(wasm));
    compiles.push(ms);
  }
  const metrics = {
    rounds: ROUNDS,
    boot_ms: round(median(rounds.map((r) => r.boot))),
    first_fetch_ms: round(median(rounds.map((r) => r.firstFetch))),
    first_do_start_commit_ms: round(median(rounds.map((r) => r.firstDo))),
    warm_fetch_p50_ms: round(median(rounds.map((r) => r.warmFetch))),
    warm_do_read_p50_ms: round(median(rounds.map((r) => r.warmDo))),
    node_wasm_compile_ms: round(median(compiles)),
    node: process.version,
    platform: `${process.platform}-${process.arch}`,
  };
  t.diagnostic(JSON.stringify(metrics));
  await writeFile(join(buildDir, 'cf1-cold-start.json'), `${JSON.stringify(metrics, null, 2)}\n`);
  // Sanity bounds only: a local stall is a failure, small numbers are not a promise.
  assert.ok(metrics.first_fetch_ms < 5000);
  assert.ok(metrics.first_do_start_commit_ms < 5000);
});
