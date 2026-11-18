// The `runbook` CLI as the pilot people and as an agent session (M6).
// Secrets go from 0600 files into the child's environment (RUNBOOK_TOKEN),
// never onto a command line or into test output.
import { execFile } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { readSecret, type E2EState, type Person } from './state.ts';

export interface CliResult {
  code: number;
  stdout: string;
  stderr: string;
}

function exec(file: string, args: string[], env: NodeJS.ProcessEnv): Promise<CliResult> {
  return new Promise((resolve) => {
    execFile(file, args, { env, maxBuffer: 16 * 1024 * 1024 }, (error, stdout, stderr) => {
      const code = error ? (typeof error.code === 'number' ? error.code : 1) : 0;
      resolve({ code, stdout, stderr });
    });
  });
}

function baseEnv(configDir: string, extra: Record<string, string> = {}): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env };
  delete env.RUNBOOK_TOKEN;
  delete env.RUNBOOK_HOST;
  return { ...env, XDG_CONFIG_HOME: configDir, ...extra };
}

export type TokenKind = 'pat' | 'seed' | 'seed-web' | 'browser' | 'editor';

/**
 * Run the CLI as a person with one of their token files:
 * `pat` (setup.sh's narrow CLI token), `seed`, `seed-web`, `browser`, or
 * `editor` (the editor person's authoring token, M5).
 */
export async function asPerson(
  state: E2EState,
  who: Person,
  args: string[],
  token: TokenKind = 'pat',
): Promise<CliResult> {
  const file =
    token === 'browser' || token === 'editor'
      ? join(state.tokensDir, `${who}.${token}`)
      : join(state.pilotDir, `${who}.${token}`);
  const env = baseEnv(join(state.pilotDir, 'config', who), { RUNBOOK_TOKEN: readSecret(file) });
  return exec(state.cli, [...args, '--host', state.apiURL], env);
}

/** Like asPerson, but throws (with stderr, which never carries a secret) on failure. */
export async function asPersonOk(
  state: E2EState,
  who: Person,
  args: string[],
  token: TokenKind = 'pat',
): Promise<string> {
  const result = await asPerson(state, who, args, token);
  if (result.code !== 0) {
    throw new Error(
      `runbook ${args.slice(0, 2).join(' ')} as ${who} exited ${result.code}: ${result.stderr}`,
    );
  }
  return result.stdout;
}

/**
 * Mint a token as `who`, authenticated with one of their seeds (which carry
 * tokens:manage), and store its secret in `file` (0600). Returns the token ID.
 * A token never exceeds its caller: `seed-web` adds runbooks:read.
 */
export async function mintToken(
  state: Pick<E2EState, 'cli' | 'pilotDir' | 'apiURL' | 'project'>,
  who: Person,
  file: string,
  options: {
    name: string;
    scopes: string[];
    agent?: boolean;
    expires?: string;
    seed?: 'seed' | 'seed-web' | 'seed-editor' | 'seed-control';
  },
): Promise<string> {
  const seed = readSecret(join(state.pilotDir, `${who}.${options.seed ?? 'seed'}`));
  const args = ['token', 'create', '--host', state.apiURL, '--name', options.name];
  for (const scope of options.scopes) args.push('--scope', scope);
  args.push('--project', state.project, '--expires', options.expires ?? '1d', '--json');
  if (options.agent) args.push('--agent');
  const result = await exec(
    state.cli,
    args,
    baseEnv(join(state.pilotDir, 'config', who), { RUNBOOK_TOKEN: seed }),
  );
  if (result.code !== 0)
    throw new Error(`token create as ${who} exited ${result.code}: ${result.stderr}`);
  const created = JSON.parse(result.stdout) as { secret: string; token: { id: string } };
  writeFileSync(file, `${created.secret}\n`, { mode: 0o600, flag: 'wx' });
  return created.token.id;
}

/** `runbook run start` as a person; returns the execution ID. */
export async function startRun(
  state: E2EState,
  who: Person,
  inputs: Record<string, string>,
  runbook = 'post-deploy',
): Promise<string> {
  const args = ['run', 'start', runbook, '--project', state.project, '--json'];
  for (const [k, v] of Object.entries(inputs)) args.push('--input', `${k}=${v}`);
  const out = await asPersonOk(state, who, args);
  return (JSON.parse(out) as { id: string }).id;
}

/** The execution's Markdown report as the CLI prints it (`run report --format md`). */
export function cliMarkdownReport(state: E2EState, who: Person, exe: string): Promise<string> {
  return asPersonOk(state, who, ['run', 'report', exe, '--format', 'md']);
}

export interface ReadyWork {
  step_run_id: string;
  path: string;
}

export interface AgentReport {
  verdict?: 'pass' | 'warn' | 'fail';
  outputs?: Record<string, string | number>;
  evidence?: string[];
  summary?: string;
}

/**
 * An agent session on one execution, the way the skill drives it: the
 * launcher's service token mints an execution-scoped pair (`run token`),
 * then every step is `step next` → `step claim` → `step get` → `step report`.
 * Claim always precedes the read (D32/F60).
 */
export class AgentDriver {
  readonly session: string;
  readonly executionId: string;
  private readonly state: E2EState;
  private readonly configDir: string;

  private constructor(state: E2EState, executionId: string) {
    this.state = state;
    this.executionId = executionId;
    this.session = `agent-${executionId}`;
    this.configDir = join(state.pilotDir, 'config', `agent-${executionId}`);
  }

  /** Waits for the run to leave `queued`, then mints the execution pair. */
  static async start(state: E2EState, executionId: string): Promise<AgentDriver> {
    const driver = new AgentDriver(state, executionId);
    mkdirSync(driver.configDir, { recursive: true, mode: 0o700 });
    await waitFor(async () => {
      const out = await asPersonOk(state, 'alice', ['run', 'get', executionId, '--json']);
      return (JSON.parse(out) as { status: string }).status !== 'queued';
    }, `${executionId} to leave queued`);
    const launcher = readSecret(join(state.pilotDir, 'launcher.svc'));
    const minted = await exec(
      state.cli,
      ['run', 'token', executionId, '--session', driver.session, '--host', state.apiURL],
      baseEnv(driver.configDir, { RUNBOOK_TOKEN: launcher }),
    );
    if (minted.code !== 0) throw new Error(`run token failed: ${minted.stderr}`);
    return driver;
  }

  private async cli(args: string[]): Promise<string> {
    const result = await exec(
      this.state.cli,
      [...args, '--host', this.state.apiURL],
      baseEnv(this.configDir),
    );
    if (result.code !== 0) {
      throw new Error(
        `agent: runbook ${args.slice(0, 2).join(' ')} exited ${result.code}: ${result.stderr}`,
      );
    }
    return result.stdout;
  }

  /** `step next --json`: the agent's ready work right now. */
  async readyWork(): Promise<ReadyWork[]> {
    const out = await this.cli(['step', 'next', this.executionId, '--json']);
    return (JSON.parse(out) as { ready_work?: ReadyWork[] }).ready_work ?? [];
  }

  /** Waits until `path` is ready for the agent; returns its step run ID. */
  async waitReady(path: string): Promise<string> {
    let found: string | undefined;
    await waitFor(async () => {
      found = (await this.readyWork()).find((w) => w.path === path)?.step_run_id;
      return Boolean(found);
    }, `${path} to be ready for the agent`);
    return found as string;
  }

  /** Claim, read, report one agent step (waits for it to be ready first). */
  async doStep(path: string, report: AgentReport = {}): Promise<string> {
    const sr = await this.waitReady(path);
    await this.cli([
      'step',
      'claim',
      sr,
      '--session',
      this.session,
      '--agent-name',
      'e2e-agent',
      '--model',
      'manual',
    ]);
    await this.cli(['step', 'get', sr]);
    const args = ['step', 'report', sr, '--status', 'succeeded'];
    if (report.verdict) args.push('--verdict', report.verdict);
    for (const [k, v] of Object.entries(report.outputs ?? {})) args.push('--output', `${k}=${v}`);
    for (const e of report.evidence ?? []) args.push('--evidence', e);
    if (report.summary) args.push('--summary', report.summary);
    await this.cli(args);
    return sr;
  }

  /**
   * The flagship's agent work up to the approval on the unhealthy path
   * (run-flagship.sh): health check, then (after the human login check) the
   * three regions with eu-west failing, then the stabilize re-check.
   */
  async smoke() {
    await this.doStep('api_health', {
      verdict: 'pass',
      outputs: { status_code: 200 },
      summary: 'GET /healthz returned 200',
    });
  }

  async regionsUnhealthy() {
    const rates = [0.002, 0.003, 0.05];
    for (const [i, rate] of rates.entries()) {
      await this.doStep(`regions[${i}].error_rate`, {
        verdict: rate > 0.01 ? 'fail' : 'pass',
        outputs: { rate },
        evidence: [`https://dashboards.example.test/errors?region=${i}`],
      });
    }
    await this.doStep('stabilize#1.recheck', { verdict: 'pass', outputs: { rate: 0.004 } });
  }
}

/** Polls `check` until true (no fixed sleeps in tests; this backs off briefly). */
export async function waitFor(
  check: () => Promise<boolean>,
  what: string,
  timeoutMs = 30_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let delay = 100;
  while (Date.now() < deadline) {
    if (await check()) return;
    await new Promise((r) => setTimeout(r, delay));
    delay = Math.min(delay * 2, 1_000);
  }
  throw new Error(`timed out waiting for ${what}`);
}

/** The step run ID of a section by path, from the execution report (JSON). */
export async function sectionStepRun(
  state: E2EState,
  who: Person,
  exe: string,
  path: string,
): Promise<string | undefined> {
  const out = await asPersonOk(state, who, ['run', 'report', exe, '--json']);
  const report = JSON.parse(out) as { sections?: { path: string; step_run_id: string }[] };
  return report.sections?.find((s) => s.path === path)?.step_run_id;
}

/** GET an API path with a token file, straight to the server (not through the page). */
export async function apiGet(state: E2EState, tokenFile: string, path: string): Promise<Response> {
  return fetch(`${state.apiURL}${path}`, {
    headers: { Authorization: `Bearer ${readSecret(tokenFile)}` },
  });
}
