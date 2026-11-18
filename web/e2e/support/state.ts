// The seeded stack as the harness describes it (e2e/.tokens/state.json).
// IDs, URLs and paths only: every secret is a separate 0600 file.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

export type Person = 'alice' | 'bob' | 'carol';

export interface E2EState {
  /** The web origin the browser opens (Vite, or E2E_BASE_URL). */
  baseURL: string;
  /** runbook-server, for the CLI and direct API calls. */
  apiURL: string;
  /** The `runbook` CLI binary. */
  cli: string;
  /** setup.sh's PILOT_DIR: owner.pat, <who>.seed, <who>.pat, launcher.svc, config/<who>/. */
  pilotDir: string;
  /** e2e/.tokens: <who>.browser, <editor>.editor, <controller>.control and tokens minted by tests. */
  tokensDir: string;
  project: string;
  projectId: string;
  users: Record<Person, string>;
  teams: { oncall: string; engLeads: string };
  /** The person with the `editor` role and `<who>.editor` (runbooks:write/publish, skills:read). */
  editor: Person;
  /**
   * The project operator with `<who>.control`: the browser scopes plus
   * executions:admin, so pause, resume and cancel are offered (D27).
   */
  controller: Person;
}

let cached: E2EState | null = null;

export function loadState(): E2EState {
  if (cached) return cached;
  const path = process.env.E2E_STATE;
  if (!path) {
    throw new Error('E2E_STATE is not set: run the suite with `npm run e2e` (see e2e/README.md).');
  }
  cached = JSON.parse(readFileSync(path, 'utf8')) as E2EState;
  return cached;
}

/** Path of a secret file; read it with readSecret, never log it. */
export function secretPath(state: E2EState, file: string): string {
  return file.includes('/') ? file : join(state.tokensDir, file);
}

export function readSecret(path: string): string {
  return readFileSync(path, 'utf8').trim();
}
