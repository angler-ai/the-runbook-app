#!/usr/bin/env node
// Validates every sample runbook in the gallery with `runbook spec validate`
// (DOCS1). Offline validation has no server directory, so it can't resolve
// teams, people, skills or called runbooks; it reports the first such
// reference and stops. A sample passes when it validates, or when its only
// diagnostic is one of those directory-resolution codes. Anything else (a
// syntax error, an unknown key, a bad expression, a limit) fails.
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { sampleFiles } from './generate.mjs';

// Complete runbooks in the hand-written pages: every ```yaml block that
// starts with `metadata:` or `apiVersion:`. Fragments (a single step) are
// not checked. The generated samples pages are skipped; their sources are.
const docs = fileURLToPath(new URL('../src/content/docs/', import.meta.url));
function pageExamples() {
  const tmp = mkdtempSync(join(tmpdir(), 'runbook-docs-'));
  const files = [];
  const walk = (dir) => {
    for (const name of readdirSync(dir)) {
      const p = join(dir, name);
      if (statSync(p).isDirectory()) {
        if (name !== 'samples' && name !== 'reference') walk(p);
      } else if (/\.mdx?$/.test(name)) {
        const blocks = [...readFileSync(p, 'utf8').matchAll(/^```yaml\n([\s\S]*?)^```$/gm)].map((m) => m[1]);
        blocks
          .filter((b) => /^(metadata|apiVersion):/.test(b))
          .forEach((b, i) => {
            const out = join(tmp, `${relative(docs, p).replace(/[\/.]/g, '_')}_${i + 1}.yaml`);
            writeFileSync(out, b);
            files.push(out);
          });
      }
    }
  };
  walk(docs);
  return { tmp, files };
}
const examples = pageExamples();
process.on('exit', () => rmSync(examples.tmp, { recursive: true, force: true }));

const NEEDS_SERVER = new Set([
  'spec.unresolved_team',
  'spec.unresolved_user',
  'spec.unresolved_skill',
  'spec.call_unresolved',
]);

const bin = process.env.RUNBOOK_BIN ? resolve(process.env.RUNBOOK_BIN) : null;
let failed = 0;
for (const file of [...sampleFiles(), ...examples.files]) {
  let out;
  try {
    out = bin
      ? execFileSync(bin, ['spec', 'validate', '--json', file], { encoding: 'utf8' })
      : execFileSync('cargo', ['run', '-q', '--locked', '-p', 'runbook-cli', '--', 'spec', 'validate', '--json', file], {
          encoding: 'utf8',
          stdio: ['ignore', 'pipe', 'inherit'],
        });
  } catch (e) {
    // `spec validate` exits non-zero for an invalid spec; its JSON is on stdout.
    out = e.stdout;
  }
  const name = relative(process.cwd(), file);
  let report;
  try {
    report = JSON.parse(out);
  } catch {
    console.error(`FAIL ${name}: no JSON report`);
    failed++;
    continue;
  }
  const blocking = report.diagnostics.filter((d) => !NEEDS_SERVER.has(d.code));
  if (report.ok) {
    console.log(`ok   ${name} (worst case ${report.worst_case_step_runs} step runs)`);
  } else if (blocking.length === 0) {
    const codes = [...new Set(report.diagnostics.map((d) => d.code))].join(', ');
    console.log(`ok   ${name} (needs a server to resolve: ${codes})`);
  } else {
    failed++;
    for (const d of blocking) console.error(`FAIL ${name}: ${d.path}: ${d.code}: ${d.message}`);
  }
}
if (failed) {
  console.error(`${failed} sample(s) failed`);
  process.exit(1);
}
