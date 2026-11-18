// Planned-operation guard (A5): UI code must never call an operation the
// contract marks x-implementation: planned. Every T0 route has shipped
// (ARC3 the archives last); the guard stays for the next planned ones.
// Runs under `node --test` because it scans source files from disk.
// Scans the API/feature/route sources for openapi-fetch calls and checks
// each against ../api/openapi.draft.json. Test files are excluded: they
// may name planned routes to assert they are not served.
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';

interface Operation {
  'x-implementation'?: string;
}
interface OpenApiDoc {
  paths: Record<string, Record<string, Operation>>;
}

const webRoot = join(import.meta.dirname, '..', '..');
const doc = JSON.parse(
  readFileSync(join(webRoot, '..', 'api', 'openapi.draft.json'), 'utf8'),
) as OpenApiDoc;

const planned = new Set<string>();
const known = new Set<string>();
for (const [path, operations] of Object.entries(doc.paths)) {
  for (const [method, operation] of Object.entries(operations)) {
    const key = `${method.toUpperCase()} ${path}`;
    known.add(key);
    if (operation['x-implementation'] === 'planned') planned.add(key);
  }
}

function* sourceFiles(dir: string): Generator<string> {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      yield* sourceFiles(path);
    } else if (
      /\.(ts|tsx)$/.test(entry.name) &&
      !entry.name.includes('.test.') &&
      entry.name !== 'schema.d.ts'
    ) {
      yield path;
    }
  }
}

test('shipped operations are not planned (ARC3 shipped the last T0 routes)', () => {
  for (const key of [
    'GET /v1/runbooks/{rb}/memory',
    'GET /v1/runbooks/{rb}/archives',
    'GET /v1/runbooks/{rb}/archives/{date}',
    'GET /v1/executions/{exe}/archive',
  ]) {
    assert.ok(known.has(key), key);
    assert.ok(!planned.has(key), key);
  }
});

test('no UI source calls a planned operation', () => {
  const callPattern = /\b(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\(\s*['"]([^'"]+)['"]/g;
  const offenders: string[] = [];
  let callsFound = 0;
  for (const dir of ['src/api', 'src/features', 'src/routes']) {
    for (const file of sourceFiles(join(webRoot, dir))) {
      const text = readFileSync(file, 'utf8');
      for (const match of text.matchAll(callPattern)) {
        const key = `${match[1]} ${match[2]}`;
        callsFound += 1;
        if (planned.has(key)) offenders.push(`${file}: ${key}`);
        // Paths are OpenAPI templates in client calls; an unknown one is
        // a typo the type checker should also catch, but flag it here.
        if (!known.has(key)) offenders.push(`${file}: unknown operation ${key}`);
      }
    }
  }
  assert.ok(callsFound > 5, `expected to find client calls, found ${callsFound}`);
  assert.deepEqual(offenders, []);
});
