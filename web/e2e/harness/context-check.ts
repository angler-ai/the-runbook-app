// U26: the image's build context must never carry the e2e harness's secrets
// (`e2e/.tokens/`, 0600 token files) or run logs (`e2e/.run/`): the
// Dockerfile's `COPY web ./` would keep them in a cached layer. Checked with
// docker itself, so `.dockerignore` is read exactly as the real build reads
// it: a throwaway BuildKit build copies `web/e2e/` from the repository's
// context into a `scratch` stage, exports it to a private temp directory (no
// image, no tag) and fails on any hidden entry. Only names are reported.
//
//   node e2e/harness/context-check.ts    # standalone (exit 1 on a leak)
//
// container.ts runs it before every image build.
import { spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const PROBE = 'FROM scratch\nCOPY web/e2e/ /e2e/\n';

/** Hidden entries (any depth) under `dir`, as paths relative to it. */
function hiddenEntries(dir: string, base = dir): string[] {
  const found: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.name.startsWith('.')) found.push(relative(base, path));
    else if (entry.isDirectory()) found.push(...hiddenEntries(path, base));
  }
  return found;
}

/**
 * Throws when docker would send any hidden `web/e2e/` entry to the image
 * build. A canary file in `e2e/.run/` (created when missing) makes the check
 * meaningful even before the harness has written anything.
 */
export async function assertContextHasNoE2ESecrets(repoRoot: string): Promise<void> {
  const runDir = join(repoRoot, 'web', 'e2e', '.run');
  mkdirSync(runDir, { recursive: true });
  writeFileSync(join(runDir, 'context-canary'), 'not a secret\n');
  const out = mkdtempSync(join(tmpdir(), 'runbook-e2e-context-'));
  try {
    const code = await new Promise<number>((r) => {
      const child = spawn(
        'docker',
        ['build', '--quiet', '--output', `type=local,dest=${out}`, '-f', '-', repoRoot],
        { stdio: ['pipe', 'ignore', 'inherit'] },
      );
      child.stdin.end(PROBE);
      child.on('exit', (exitCode, signal) => r(exitCode ?? (signal ? 130 : 1)));
    });
    if (code !== 0) throw new Error(`the build-context probe (docker build) exited ${code}`);
    const leaked = hiddenEntries(join(out, 'e2e'));
    if (leaked.length > 0) {
      throw new Error(
        `.dockerignore lets the e2e harness's files into the image build context: web/e2e/${leaked.join(', web/e2e/')}`,
      );
    }
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
  assertContextHasNoE2ESecrets(repoRoot).then(
    () => process.stdout.write('[e2e] build context: no web/e2e hidden entries\n'),
    (error: unknown) => {
      process.stderr.write(`[e2e] ${error instanceof Error ? error.message : String(error)}\n`);
      process.exit(1);
    },
  );
}
