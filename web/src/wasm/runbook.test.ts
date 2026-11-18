// Loads the BUILT browser compiler (src/wasm/runbook.wasm from `npm run wasm:build`)
// and checks it against the outline fixture the Graph and Outline views use.
// Without the module (no Rust toolchain) these tests skip; `npm run check`
// builds it first whenever cargo is available.
import { describe, expect, it } from 'vitest';
// A byte-for-byte mirror of conformance/spec/post-deploy.yaml (Vite serves only
// web/; crates/runbook-web-wasm/tests/outline_fixture.rs pins the mirror; .txt
// keeps Prettier from reformatting it).
import flagship from './fixtures/post-deploy.yaml.txt?raw';
import fixture from '../ui/outline/fixtures/post-deploy.outline.json';
import { CompilerError, loadCompiler, type RunbookCompiler } from './runbook';

// Detect the built module on disk (an import.meta.glob of a gitignored
// binary did not match, so these tests silently never ran). The test runs in
// Vitest on Node; the app's TS project has no Node types, hence the
// dynamically named import.
type FsPromises = {
  readFile: (path: string) => Promise<Uint8Array<ArrayBuffer>>;
  access: (path: string) => Promise<void>;
};
const fs = (await import(/* @vite-ignore */ 'node:fs/promises' as string)) as FsPromises;
// A path string (jsdom replaces the global URL class, which Node's fs rejects).
const here = decodeURIComponent(new URL(import.meta.url).pathname);
const wasmPath = here.slice(0, here.lastIndexOf('/') + 1) + 'runbook.wasm';
const isBuilt = await fs.access(wasmPath).then(
  () => true,
  () => false,
);
const skipReason = 'src/wasm/runbook.wasm is not built (run `npm run wasm:build`)';
if (!isBuilt) console.warn(`SKIPPED wasm tests: ${skipReason}`);

async function compiler(): Promise<RunbookCompiler> {
  return loadCompiler(await fs.readFile(wasmPath));
}

const directory = {
  teams: [
    { id: 'team_01ARZ3NDEKTSV4RRFFQ69G5FAV', slug: 'oncall' },
    { id: 'team_01ARZ3NDEKTSV4RRFFQ69G5FAW', slug: 'eng-leads' },
  ],
  skills: [
    { name: 'check-error-rates', hash: '01'.repeat(32) },
    { name: 'rollback-service', hash: '02'.repeat(32) },
  ],
};

describe.skipIf(!isBuilt)(`built runbook.wasm${isBuilt ? '' : ` (skipped: ${skipReason})`}`, () => {
  it('outlines the flagship exactly as the fixture', async () => {
    const c = await compiler();
    const result = c.outline({ source: flagship });
    expect(result.diagnostics).toEqual([]);
    expect(result.outline).toEqual(fixture);
  });

  it('validates with the server body shape and describes the start form', async () => {
    const c = await compiler();
    expect(c.validate({ source: flagship, context: directory })).toEqual({
      diagnostics: [],
      step_budget: 20,
    });
    const bare = c.validate({ source: flagship });
    expect(bare.step_budget).toBeNull();
    expect(bare.diagnostics.length).toBeGreaterThan(0);
    const broken = c.validate({ source: 'apiVersion: therunbook.app/v1\nmetadata: [unclosed\n' });
    expect(broken.diagnostics[0]?.line).toBeGreaterThanOrEqual(1);

    const described = c.describe({ source: flagship });
    expect(described.runbook?.id).toBe('post-deploy');
    expect(described.runbook?.title).toBe('Post-deploy verification');
    expect(described.runbook?.inputs).toMatchObject({
      type: 'object',
      required: ['service', 'version'],
    });
    expect(c.describe({ source: 'kind: Runbook\n' }).runbook).toBeNull();
  });

  it('rejects bad calls with a CompilerError and keeps working', async () => {
    const c = await compiler();
    expect(() => c.validate({ source: ' '.repeat(1_048_577) })).toThrow(CompilerError);
    try {
      c.validate({ source: flagship, context: { variables: { x: { type: 'nope' } } } });
      expect.unreachable();
    } catch (error) {
      expect((error as CompilerError).code).toBe('vars.invalid_stored_declarations');
    }
    // Many calls: memory growth must not corrupt results.
    for (let i = 0; i < 50; i++) expect(c.outline({ source: flagship }).outline).toEqual(fixture);
  });

  // What the builder's Triggers form writes compiles cleanly, and a bad
  // cron comes back at the path the form shows it under.
  it('takes the triggers the builder writes', async () => {
    const c = await compiler();
    const source = (cron: string) => `metadata:
  id: nightly-checks
  title: Nightly checks
spec:
  inputs:
    service: { type: string, required: true }
    replicas: { type: integer, default: 2 }
    region: { type: enum, options: [us, eu] }
  triggers:
    schedule:
      - id: daily
        cron: "${cron}"
        timezone: America/New_York
        catch_up: 1h
        inputs:
          service: api
          replicas: 3
          region: eu
    webhook:
      - id: incoming
        payload:
          status: { type: string }
          repo: { type: string }
        when: "\${{ payload.status == 'ok' }}"
        inputs:
          service: "\${{ payload.repo }}"
        dedupe: "\${{ payload.repo }}"
  steps:
    - id: health
      title: Health answers 200
`;
    expect(c.validate({ source: source('30 8 * * 1-5'), context: directory }).diagnostics).toEqual(
      [],
    );
    const bad = c.validate({ source: source('61 8 * * *'), context: directory }).diagnostics;
    expect(bad.map((d) => [d.code, d.path])).toEqual([
      ['spec.invalid_cron', '/spec/triggers/schedule/0/cron'],
    ]);
  });
});
