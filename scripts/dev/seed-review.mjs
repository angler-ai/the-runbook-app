// Seed for the UI-1 review stack (scripts/dev/review.sh): an organization
// as `runbook-server bootstrap` made it, turned into a small platform team
// with realistic runbooks and runs. Everything goes through the `runbook`
// CLI or the public API, as the owner, a project service token, or an
// agent session holding an execution-scoped pair (`runbook run token`).
// No SQL.
//
// Environment: REVIEW_STATE_DIR (0700; holds bootstrap.json), RUNBOOK_BIN,
// REVIEW_API_URL. Writes, in the state dir: owner.pat, launcher.svc,
// browser.token and browser.header (0600 secrets), and state.json (IDs only).
// Secrets go from files into a child's environment, never onto argv or
// stdout. Lines starting `summary: ` are what review.sh prints.
import { execFile } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const STATE = need('REVIEW_STATE_DIR');
const CLI = need('RUNBOOK_BIN');
const API = need('REVIEW_API_URL').replace(/\/$/, '');
const RUNBOOKS = join(dirname(fileURLToPath(import.meta.url)), 'review-runbooks');

function need(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}
const log = (message) => process.stdout.write(`${message}\n`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function writeSecret(name, secret) {
  writeFileSync(join(STATE, name), `${secret}\n`, { mode: 0o600, flag: 'wx' });
}
const readSecret = (name) => readFileSync(join(STATE, name), 'utf8').trim();

// --- CLI and API -------------------------------------------------------------------

/**
 * `runbook <args> --host API` with its own config dir (`config/<who>`) and,
 * when `token` names a secret file, that token in RUNBOOK_TOKEN.
 */
function cli(who, args, { token, input, allowFailure = false } = {}) {
  const env = { ...process.env, XDG_CONFIG_HOME: join(STATE, 'config', who) };
  delete env.RUNBOOK_TOKEN;
  delete env.RUNBOOK_HOST;
  if (token) env.RUNBOOK_TOKEN = readSecret(token);
  mkdirSync(env.XDG_CONFIG_HOME, { recursive: true, mode: 0o700 });
  return new Promise((resolve, reject) => {
    const child = execFile(
      CLI,
      [...args, '--host', API],
      { env, maxBuffer: 16 * 1024 * 1024 },
      (error, stdout, stderr) => {
        if (error && allowFailure && typeof error.code === 'number') {
          resolve(stdout);
        } else if (error) {
          // stderr never carries a secret; argv here never does either.
          reject(new Error(`runbook ${args.slice(0, 3).join(' ')} (as ${who}) failed: ${stderr || error.message}`));
        } else {
          resolve(stdout);
        }
      },
    );
    child.stdin.end(input ?? '');
  });
}
const cliJson = async (who, args, opts) => JSON.parse(await cli(who, [...args, '--json'], opts));
const owner = (args) => cliJson('owner', args, { token: 'owner.pat' });

async function api(method, path, body, token = 'owner.pat') {
  const headers = { Authorization: `Bearer ${readSecret(token)}` };
  if (body !== undefined) {
    headers['Content-Type'] = 'application/json';
    headers['Idempotency-Key'] = randomUUID();
  }
  const response = await fetch(`${API}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`${method} ${path} -> ${response.status}: ${text}`);
  return text ? JSON.parse(text) : null;
}

async function waitFor(what, check, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let delay = 100;
  while (Date.now() < deadline) {
    const value = await check();
    if (value) return value;
    await sleep(delay);
    delay = Math.min(delay * 2, 1_000);
  }
  throw new Error(`timed out waiting for ${what}`);
}

// --- Identities ------------------------------------------------------------------

const boot = JSON.parse(readFileSync(join(STATE, 'bootstrap.json'), 'utf8'));
writeSecret('owner.pat', boot.pat);
// bootstrap.json keeps only IDs from here on.
writeFileSync(
  join(STATE, 'bootstrap.json'),
  `${JSON.stringify({ org_id: boot.org_id, user_id: boot.user_id, project_id: boot.project_id })}\n`,
  { mode: 0o600 },
);
const ORG = boot.org_id;
const OWNER_ID = boot.user_id;

log('projects: ops (Platform Ops) and sandbox (Sandbox)');
const ops = (await owner(['project', 'create', '--slug', 'ops', '--title', 'Platform Ops'])).id;
const sandbox = (await owner(['project', 'create', '--slug', 'sandbox', '--title', 'Sandbox'])).id;

log('people and teams');
const PEOPLE = {
  priya: 'Priya Natarajan',
  marcus: 'Marcus Okafor',
  elena: 'Elena Ruiz',
  sam: 'Sam Whitaker',
  jin: 'Jin Park',
};
const users = { riley: OWNER_ID };
for (const [key, name] of Object.entries(PEOPLE)) {
  users[key] = (await owner(['user', 'create', '--name', name])).id;
  await owner(['project', 'member', 'add', ops, users[key], '--role', 'operator']);
}
await owner(['project', 'member', 'add', sandbox, users.marcus, '--role', 'operator']);
const TEAMS = {
  oncall: ['On-call', ['riley', 'marcus', 'jin']],
  'customer-success': ['Customer success', ['riley', 'priya', 'elena']],
  solutions: ['Solutions engineering', ['sam', 'elena']],
};
const teams = {};
for (const [slug, [title, members]] of Object.entries(TEAMS)) {
  teams[slug] = (await owner(['team', 'create', '--slug', slug, '--title', title])).id;
  for (const member of members) await owner(['team', 'member', 'add', teams[slug], users[member]]);
}

log('service token for ops (agent launcher and integrations)');
// POST /v1/tokens, as examples/pilot/setup.sh mints its launcher.
const svc = await api('POST', '/v1/tokens', {
  kind: 'service',
  name: 'ops-automation',
  scopes: ['executions:read', 'executions:act'],
  projects: [ops],
  role: 'operator',
});
writeSecret('launcher.svc', svc.secret);

// --- Runbooks --------------------------------------------------------------------

// Offline validation has no directory or catalog: people, teams and `call`
// targets resolve only on the server (POST /v1/validate below checks them).
const offlineOnlyUnresolved = (d) =>
  /^spec\.(unresolved_(team|user)|call_(unresolved|cycle|depth_exceeded|unknown_input|missing_input|input_type|unknown_output|output_type))$/.test(
    d.code,
  );

/** `spec validate` (offline) and POST /v1/validate (resolves people and teams). */
async function validate(file) {
  // Exits 1 with the JSON report when the spec has diagnostics.
  const offline = JSON.parse(
    await cli('owner', ['spec', 'validate', file, '--json'], { allowFailure: true }),
  );
  const blocking = (offline.diagnostics ?? []).filter((d) => !offlineOnlyUnresolved(d));
  if (blocking.length > 0) throw new Error(`${file}: ${JSON.stringify(blocking)}`);
  const source = readFileSync(file, 'utf8');
  const server = await api('POST', '/v1/validate', { source, project: ops });
  if (server.diagnostics.length > 0) throw new Error(`${file}: ${JSON.stringify(server.diagnostics)}`);
}

/** Save `file` as the draft and publish it; returns the version number. */
async function publish(file) {
  await validate(file);
  await cli('owner', ['push', file, '--project', 'ops', '--draft'], { token: 'owner.pat' });
  const slug = /^\s+id:\s*(\S+)/m.exec(readFileSync(file, 'utf8'))[1];
  const published = await owner(['publish', slug, '--project', 'ops']);
  return published.runbook_version;
}
async function draft(file) {
  await validate(file);
  await cli('owner', ['push', file, '--project', 'ops', '--draft'], { token: 'owner.pat' });
}
const rb = (name) => join(RUNBOOKS, name);

// --- Runs ------------------------------------------------------------------------

async function start(slug, inputs, title) {
  const args = ['run', 'start', slug, '--project', 'ops'];
  for (const [k, v] of Object.entries(inputs)) args.push('--input', `${k}=${v}`);
  if (title) args.push('--title', title);
  const { id } = await owner(args);
  await waitFor(`${id} to leave queued`, async () => (await owner(['run', 'get', id])).status !== 'queued');
  return id;
}
const status = async (exe) => (await owner(['run', 'get', exe])).status;

/** An agent session on one run: the service token mints its execution pair. */
async function agent(exe, name = 'ops-agent') {
  const who = `agent-${exe}`;
  const session = `sess-${exe.slice(-8).toLowerCase()}`;
  // Leaving the queue is admission, not activation: minting before the
  // run has actually started answers execution.not_started.
  await waitFor(`${exe} to start`, async () => {
    const run = await owner(['run', 'get', exe]);
    return run.started_at_ms != null ? true : undefined;
  });
  await cli(who, ['run', 'token', exe, '--session', session], { token: 'launcher.svc' });
  const ready = (path) =>
    waitFor(`${path} to be ready for the agent on ${exe}`, async () => {
      const brief = await cliJson(who, ['step', 'next', exe]);
      return brief.ready_work?.find((w) => w.path === path)?.step_run_id;
    }, 90_000); // a loop's delay (UI-3: 20 s) comes before its next iteration
  return {
    who,
    /**
     * Claim, read, log, report one step. `report: false` leaves it claimed
     * and mid-way (logs only).
     */
    async step(path, { logs = [], verdict, outputs = {}, evidence = [], summary, status = 'succeeded', report = true } = {}) {
      const sr = await ready(path);
      await cli(who, ['step', 'claim', sr, '--session', session, '--agent-name', name, '--model', 'claude-sonnet']);
      await cli(who, ['step', 'get', sr]);
      if (logs.length > 0) await cli(who, ['step', 'log', sr], { input: `${logs.join('\n')}\n` });
      if (!report) return sr;
      const args = ['step', 'report', sr, '--status', status];
      if (verdict) args.push('--verdict', verdict);
      for (const [k, v] of Object.entries(outputs)) args.push('--output', `${k}=${v}`);
      for (const e of evidence) args.push('--evidence', e);
      if (summary) args.push('--summary', summary);
      await cli(who, args);
      return sr;
    },
  };
}

/** The owner (Riley) does a person's step: claim, tick the checklist, report. */
async function personStep(exe, path, { tick = [], summary, verdict } = {}) {
  const sr = await waitFor(`${path} to be ready for Riley on ${exe}`, async () => {
    const brief = await owner(['step', 'next', exe]);
    return brief.ready_work?.find((w) => w.path === path)?.step_run_id;
  });
  await cli('owner', ['step', 'claim', sr, '--session', 'riley-web'], { token: 'owner.pat' });
  if (tick.length > 0) {
    const args = ['step', 'checklist', sr];
    for (const t of tick) args.push('--tick', t);
    await cli('owner', args, { token: 'owner.pat' });
  }
  const args = ['step', 'report', sr, '--status', 'succeeded'];
  if (verdict) args.push('--verdict', verdict);
  if (summary) args.push('--summary', summary);
  await cli('owner', args, { token: 'owner.pat' });
}

async function sectionStepRun(exe, path) {
  return waitFor(`${path} on ${exe}`, async () => {
    const report = await owner(['run', 'report', exe]);
    return report.sections?.find((s) => s.path === path)?.step_run_id;
  });
}

/** An integration (the service token) reports a step itself. */
async function serviceStep(exe, path, { verdict, outputs = {}, summary, logs = [] }) {
  const sr = await waitFor(`${path} to be ready for the service on ${exe}`, async () => {
    const brief = await cliJson('svc', ['step', 'next', exe], { token: 'launcher.svc' });
    return brief.ready_work?.find((w) => w.path === path)?.step_run_id;
  });
  const opts = { token: 'launcher.svc' };
  await cli('svc', ['step', 'claim', sr, '--session', 'ingestion-api', '--agent-name', 'ingestion-api'], opts);
  await cli('svc', ['step', 'get', sr], opts);
  if (logs.length > 0) await cli('svc', ['step', 'log', sr], { ...opts, input: `${logs.join('\n')}\n` });
  const args = ['step', 'report', sr, '--status', 'succeeded'];
  if (verdict) args.push('--verdict', verdict);
  for (const [k, v] of Object.entries(outputs)) args.push('--output', `${k}=${v}`);
  if (summary) args.push('--summary', summary);
  await cli('svc', args, opts);
}

const finished = (exe) =>
  waitFor(`${exe} to finish`, async () => {
    const s = await status(exe);
    return ['succeeded', 'failed', 'cancelled'].includes(s) ? s : null;
  });

const runs = [];
const record = async (slug, exe, intent) => {
  runs.push({ runbook: slug, id: exe, intended: intent, status: await status(exe) });
};

// --- Post-deploy checks: v1, v2 and v3, five runs --------------------------------

/** One post-deploy run's agent work. `scenario`: healthy, eu-west-fails, running. */
async function postDeploy(exe, { service, version, scenario, latency = true, before = true }) {
  const a = await agent(exe, 'deploy-checker');
  const host = `${service}.internal`;
  await a.step('health', {
    logs: [
      `GET https://${host}/healthz -> 200 in 84ms`,
      `GET https://${host}/healthz -> 200 in 71ms`,
      `GET https://${host}/healthz -> 200 in 92ms`,
      `build=${version} uptime=4m12s replicas=12/12 ready`,
    ],
    verdict: 'pass',
    outputs: { status_code: 200, latency_ms: 92 },
    summary: `3/3 health checks answered 200; slowest 92 ms. All 12 replicas report ${version}.`,
  });
  const rates =
    scenario === 'eu-west-fails'
      ? { 'us-east': 0.0021, 'us-west': 0.0034, 'eu-west': 0.0241 }
      : { 'us-east': 0.0018, 'us-west': 0.0026, 'eu-west': 0.0041 };
  let i = 0;
  for (const [region, rate] of Object.entries(rates)) {
    const pct = (rate * 100).toFixed(2);
    const verdict = rate > 0.01 ? 'fail' : rate > 0.005 ? 'warn' : 'pass';
    const logs = [
      `querying error_rate{service="${service}",region="${region}"} over 10m`,
      `baseline (previous 24h, same window): ${(rate * 100 * (verdict === 'fail' ? 0.18 : 0.9)).toFixed(2)}%`,
      `5xx=${Math.round(rate * 184_000)} of 184000 requests -> error_rate=${pct}%`,
    ];
    if (verdict === 'fail') {
      // UI-3: a realistic agent log for the failing check (the run page's
      // log block and the Document view read it).
      const minutes = [0.0031, 0.0058, 0.0112, 0.0187, 0.0243, 0.0261, 0.0249, 0.0238, 0.0241, 0.0236];
      minutes.forEach((r, m) => logs.push(`  t-${10 - m}m  5xx=${(r * 100).toFixed(2)}%  rps=${300 + m * 4}`));
      logs.push('breakdown by upstream (last 10m):');
      logs.push('  payments-gateway eu-west-1   71.4%  upstream connect error or disconnect/reset before headers');
      logs.push('  ledger eu-west-1             18.9%  503 from upstream (circuit open)');
      logs.push('  search eu-west-1              9.7%  504 gateway timeout');
      logs.push('payments-gateway pool: max_connections=64, in_use=64, pending=412 (saturated since t-8m)');
      logs.push('diff from previous release: http.client.pool_timeout 2s -> 10s (config/payments.yaml)');
      logs.push('us-east and us-west run the same build with lower traffic; neither shows pool saturation');
      logs.push('error_rate above the 1% threshold for 7 consecutive minutes -> verdict fail');
      logs.push('recommendation: hold the roll-forward; revert pool_timeout or raise max_connections in eu-west');
    }
    await a.step(`error_rates[${i}].error_rate`, {
      logs,
      verdict,
      outputs: { rate },
      evidence: [`https://grafana.internal/d/svc-errors?var-service=${service}&var-region=${region}&from=now-15m`],
      summary:
        verdict === 'fail'
          ? `${region} error rate ${pct}% (threshold 1%), mostly upstream connect errors to payments-gateway.`
          : `${region} error rate ${pct}%, within the 0.5% target.`,
    });
    i += 1;
  }
  if (!latency) return a;
  const p95 = scenario === 'eu-west-fails' ? 468 : 412;
  const latencyStep = {
    logs: [
      `querying histogram_quantile(0.95, http_request_duration_seconds{service="${service}"}) over 15m`,
      `p95=${p95}ms (threshold 500ms)`,
      `p95 before deploy=388ms (+${Math.round((p95 / 388 - 1) * 100)}%)`,
    ],
    verdict: scenario === 'eu-west-fails' ? 'warn' : 'pass',
    outputs: before ? { p95_ms: p95, p95_before_ms: 388 } : { p95_ms: p95 },
    summary: `p95 ${p95} ms against a 500 ms threshold (${Math.round((p95 / 388 - 1) * 100)}% above the 388 ms before the deploy).`,
  };
  if (scenario === 'running') {
    // Mid-way: claimed, first log lines in, no report yet. keep-claimed.mjs
    // heartbeats the claim so it outlives the 10-minute agent lease.
    const sr = await a.step('latency', { logs: latencyStep.logs.slice(0, 1), report: false });
    writeFileSync(
      join(STATE, 'running.json'),
      `${JSON.stringify({ execution_id: exe, step_run_id: sr, who: a.who })}\n`,
      { mode: 0o600 },
    );
    return a;
  }
  await a.step('latency', latencyStep);
  return a;
}
async function postDeploySummary(a, text) {
  await a.step('summary', { summary: text, logs: ['posting summary to #deploys'] });
}

log('runbook post-deploy-checks');
let v = await publish(rb('versions/post-deploy-checks.v1.yaml'));
log(`  published v${v}`);
{
  const exe = await start('post-deploy-checks', { service: 'api', version: 'v2026.11.09' });
  const a = await postDeploy(exe, { service: 'api', version: 'v2026.11.09', scenario: 'healthy', latency: false });
  await postDeploySummary(a, 'api v2026.11.09: health pass, error rates pass in us-east, us-west and eu-west. Nothing to do.');
  await finished(exe);
  await record('post-deploy-checks', exe, 'succeeded (v1)');
}
v = await publish(rb('versions/post-deploy-checks.v2.yaml'));
log(`  published v${v}`);
{
  const exe = await start('post-deploy-checks', { service: 'web', version: 'v2026.11.10' });
  const a = await postDeploy(exe, { service: 'web', version: 'v2026.11.10', scenario: 'healthy', before: false });
  await postDeploySummary(a, 'web v2026.11.10: all checks pass; p95 412 ms. Nothing to do.');
  await finished(exe);
  await record('post-deploy-checks', exe, 'succeeded (v2)');
}
v = await publish(rb('post-deploy-checks.yaml'));
log(`  published v${v}`);
{
  const exe = await start('post-deploy-checks', { service: 'worker', version: 'v2026.11.12' });
  const a = await postDeploy(exe, { service: 'worker', version: 'v2026.11.12', scenario: 'healthy' });
  await postDeploySummary(a, 'worker v2026.11.12: all checks pass; p95 412 ms. Nothing to do.');
  await finished(exe);
  await record('post-deploy-checks', exe, 'succeeded');
}
{
  const exe = await start('post-deploy-checks', { service: 'api', version: 'v2026.11.13' });
  const a = await postDeploy(exe, { service: 'api', version: 'v2026.11.13', scenario: 'eu-west-fails' });
  await postDeploySummary(
    a,
    'api v2026.11.13 FAILED: eu-west error rate 2.41% (threshold 1%), 71% upstream connect errors to payments-gateway; us-east and us-west pass; p95 468 ms (warn). Look at the payments-gateway connection pool in eu-west before rolling forward.',
  );
  await finished(exe);
  await record('post-deploy-checks', exe, 'failed on the eu-west error rate');
}
{
  const exe = await start('post-deploy-checks', { service: 'billing', version: 'v2026.11.13-2' });
  await postDeploy(exe, { service: 'billing', version: 'v2026.11.13-2', scenario: 'running' });
  await record('post-deploy-checks', exe, 'running: agent has claimed latency and is mid-way');
}

// --- Release train (UI-2): calls post-deploy-checks, so it is published after it.
// One high-risk run: an agent clears preflight and the build, and the run
// waits on the change advisory review (team oncall, Riley among them).

log('runbook release-train');
v = await publish(rb('release-train.yaml'));
log(`  published v${v}`);
{
  // UI-3: a low-risk release end to end, so the run page's graph shows the
  // default branch taken, a `join: any` canary, three regions (eu-west
  // settles on its second loop iteration), the call's child run, a person,
  // a customer signal and the finally.
  const version = 'v2026.11.13';
  const exe = await start('release-train', { version, risk: 'low' });
  const a = await agent(exe, 'release-agent');
  await a.step('changelog', {
    logs: ['31 merged PRs since v2026.11.12, all listed', 'release notes: https://docs.internal/releases/v2026.11.13'],
    verdict: 'pass',
    summary: `Changelog complete for ${version}; release notes linked.`,
  });
  await a.step('migrations', {
    logs: ['dry run on replica-2: no pending migrations'],
    verdict: 'pass',
    outputs: { pending: 0 },
    summary: 'No migrations in this release.',
  });
  await a.step('freeze', { logs: ['change calendar: no freeze; no open SEV1/SEV2'], verdict: 'pass', summary: 'No freeze and no major incident.' });
  await a.step('build', {
    logs: ['built 6 images in 4m02s', 'signed with cosign; digest recorded', 'SBOM attached to each image'],
    outputs: { digest: 'sha256:9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b' },
    summary: 'Six images built, signed and recorded.',
  });
  await a.step('canary_us', {
    logs: ['deployed to 5% of us-east (12 of 240 pods)', 'canary 5xx=0.08% baseline 5xx=0.09% over 15m', 'canary p95=201ms baseline p95=198ms'],
    verdict: 'pass',
    summary: 'us-east canary matches the baseline on errors and latency.',
  });
  // join: any — the eu-west canary is cancelled once us-east passes.
  const regions = { 'us-east': [0.0041], 'us-west': [0.0062], 'eu-west': [0.0213, 0.0048] };
  // Foreach keys are the items' indexes: rollout[0] is us-east.
  for (const [index, [region, rates]] of Object.entries(regions).entries()) {
    await a.step(`rollout[${index}].deploy_region`, {
      logs: [`rolling ${version} to ${region}: 240 pods, maxSurge 25%`, 'rollout complete in 6m18s; 0 restarts'],
      summary: `${version} is on every instance in ${region}.`,
    });
    for (const [i, rate] of rates.entries()) {
      await a.step(`rollout[${index}].settle#${i + 1}.region_health`, {
        logs: [`5xx rate in ${region} over the last 5m: ${(rate * 100).toFixed(2)}%`],
        verdict: rate > 0.01 ? 'fail' : rate > 0.005 ? 'warn' : 'pass',
        outputs: { error_rate: rate },
        summary:
          rate > 0.01
            ? `${region} at ${(rate * 100).toFixed(2)}% right after the deploy; waiting for it to settle.`
            : `${region} settled at ${(rate * 100).toFixed(2)}%.`,
      });
    }
  }
  const view = await waitFor(`${exe} to call post-deploy-checks`, async () => {
    const run = await owner(['run', 'get', exe]);
    const call = (run.steps ?? []).find((step) => step.path === 'verify')?.call;
    return call?.started ? call.child_execution_id : null;
  }, 60_000);
  const child = await postDeploy(view, { service: 'api', version, scenario: 'healthy' });
  await postDeploySummary(child, `api ${version}: health pass, error rates pass in all three regions, p95 412 ms.`);
  await finished(view);
  await record('post-deploy-checks', view, 'succeeded (called by the low-risk release train)');
  await personStep(exe, 'status_page', {
    tick: ['Maintenance closed', 'Release notes linked'],
    summary: 'Maintenance window closed on the status page; release notes linked.',
  });
  await api('POST', `/v1/executions/${exe}/signals/release_email_sent`, { payload: { campaign: 'release-v2026-11-13' } }, 'launcher.svc');
  await a.step('retro_notes', {
    logs: ['collected run outcome, canary and per-region results', 'posted summary to #releases'],
    summary: `${version} shipped to us-east, us-west and eu-west. eu-west took a second health check to settle (2.13% then 0.48%). No rollbacks; no follow-ups.`,
  });
  await finished(exe);
  await record('release-train', exe, 'succeeded (low risk, end to end)');
}
{
  const exe = await start('release-train', { version: 'v2026.11.14', risk: 'high' });
  const a = await agent(exe, 'release-agent');
  await a.step('changelog', {
    logs: ['42 merged PRs, all listed; release notes linked'],
    verdict: 'pass',
    summary: 'Changelog complete for v2026.11.14.',
  });
  await a.step('migrations', {
    logs: ['dry run on replica-2: 2 pending migrations, both additive'],
    verdict: 'pass',
    outputs: { pending: 2 },
    summary: 'Two additive migrations; safe to roll forward and back.',
  });
  await a.step('freeze', {
    logs: ['change calendar: no freeze; no open SEV1/SEV2'],
    verdict: 'pass',
    summary: 'No freeze and no major incident.',
  });
  await a.step('build', {
    logs: ['built 6 images', 'signed with cosign; digest recorded'],
    outputs: { digest: 'sha256:5f1c0e2a9b7d4c3e8f6a1b2c3d4e5f60718293a4b5c6d7e8f9a0b1c2d3e4f5a6' },
    summary: 'Images built and signed.',
  });
  await waitFor(`${exe} to wait on the change advisory review`, async () => (await status(exe)) === 'waiting');
  await record('release-train', exe, 'waiting on the change advisory review (team oncall)');
}
{
  // UI-3: the train runs one at a time (concurrency limit 1, queue): the
  // next release waits for a slot.
  const { id } = await owner([
    'run', 'start', 'release-train', '--project', 'ops',
    '--input', 'version=v2026.11.15', '--input', 'risk=medium',
  ]);
  await record('release-train', id, 'queued behind the high-risk release');
}

// --- Customer onboarding: v2, three runs ------------------------------------------

log('runbook customer-onboarding');
await publish(rb('versions/customer-onboarding.v1.yaml'));
await publish(rb('customer-onboarding.yaml'));

async function onboardingStart(customer, plan, region, ws, rows) {
  const exe = await start('customer-onboarding', { customer, plan, region });
  await personStep(exe, 'kickoff', {
    tick: ['Goals and success criteria agreed', 'Technical contact named', 'Go-live date proposed'],
    summary: `Kickoff done. Go-live target in two weeks; technical contact is their head of engineering.`,
  });
  const a = await agent(exe, 'onboarding-agent');
  await a.step('provision', {
    logs: [
      `creating workspace for "${customer}" in ${region} (plan=${plan})`,
      `workspace ${ws} created; seeding default roles`,
      'invited technical contact as admin',
    ],
    outputs: { workspace_id: ws },
    summary: `Workspace ${ws} created in ${region}; admin invite sent.`,
  });
  await a.step('import_history', {
    logs: [
      'downloading export orders-2025-11..2026-10.csv.gz (412 MB)',
      'validating schema: 31 columns, 0 unknown',
      `imported ${rows} rows in 6m41s (0 rejected)`,
    ],
    outputs: { rows_imported: rows },
    summary: `Imported ${rows.toLocaleString('en-US')} orders from the last 12 months.`,
  });
  return { exe, a };
}

{
  // In progress: SSO (solutions), billing (Priya) and the customer's store connection pending.
  const { exe } = await onboardingStart('Northwind Outfitters', 'enterprise', 'eu', 'ws_nw7f3k2q', 1284330);
  // UI-3: billing is under way (claimed, one item ticked), so the parallel
  // setup shows done, in progress, ready and waiting-on-a-signal at once.
  const billing = await sectionStepRun(exe, 'billing');
  const claimed = await cli('owner', ['step', 'claim', billing, '--session', 'riley-web'], { token: 'owner.pat', allowFailure: true });
  if (claimed) await cli('owner', ['step', 'checklist', billing, '--tick', 'Subscription created'], { token: 'owner.pat', allowFailure: true });
  await record('customer-onboarding', exe, 'waiting: SSO (team solutions) ready, billing in progress, the customer signal pending');
}
{
  // Waiting on the go-live approval.
  const { exe } = await onboardingStart('Bluebird Coffee', 'growth', 'us', 'ws_bb2m9x4c', 86412);
  await personStep(exe, 'sso', {
    tick: ['SAML metadata exchanged', 'Test sign-in works', 'Just-in-time provisioning on'],
    summary: 'Okta SAML configured; test user signed in.',
  });
  await personStep(exe, 'billing', {
    tick: ['Subscription created', 'Invoice contact confirmed'],
    summary: 'Growth subscription active; first invoice sent.',
  });
  await api('POST', `/v1/executions/${exe}/signals/store_connected`, { payload: { platform: 'shopify', store: 'bluebird-coffee' } }, 'launcher.svc');
  await serviceStep(exe, 'first_events', {
    verdict: 'pass',
    outputs: { events: 3812 },
    logs: ['first event received from store bluebird-coffee', '3812 events in the first hour; 0 schema errors'],
    summary: '3,812 live events in the first hour, no schema errors.',
  });
  await record('customer-onboarding', exe, 'waiting on the go-live approval (team customer-success)');
}
{
  // Done end to end.
  const { exe, a } = await onboardingStart('Harbor & Pine', 'starter', 'us', 'ws_hp4r8n1v', 12907);
  await personStep(exe, 'sso', {
    tick: ['SAML metadata exchanged', 'Test sign-in works', 'Just-in-time provisioning on'],
    summary: 'Google Workspace SAML configured.',
  });
  await personStep(exe, 'billing', {
    tick: ['Subscription created', 'Invoice contact confirmed'],
    summary: 'Starter subscription active.',
  });
  await api('POST', `/v1/executions/${exe}/signals/store_connected`, { payload: { platform: 'woocommerce', store: 'harbor-and-pine' } }, 'launcher.svc');
  await serviceStep(exe, 'first_events', {
    verdict: 'pass',
    outputs: { events: 642 },
    summary: '642 live events in the first hour.',
  });
  const approval = await sectionStepRun(exe, 'go_live');
  await cli('owner', ['step', 'decide', approval, '--option', 'go_live', '--comment', 'All green; going live Monday.'], { token: 'owner.pat' });
  await a.step('welcome', {
    logs: ['sent welcome pack to 2 admins', 'scheduled first-week check-in for Thursday'],
    summary: 'Welcome pack sent to both admins; check-in booked for Thursday.',
  });
  await finished(exe);
  await record('customer-onboarding', exe, 'succeeded');
}

// --- Incident triage: v2 published, a draft ahead, one run waiting on on-call -------

log('runbook incident-triage');
await publish(rb('versions/incident-triage.v1.yaml'));
await publish(rb('incident-triage.yaml'));
await draft(rb('drafts/incident-triage.yaml'));
{
  const exe = await start('incident-triage', { summary: 'Checkout 502s in eu-west', service: 'api' });
  const a = await agent(exe, 'triage-agent');
  await a.step('gather', {
    logs: [
      'alerts firing: CheckoutErrorRateHigh (eu-west), PaymentsGatewayLatency (eu-west), SLOBurnFast (api)',
      'deploys in the last 2h: api v2026.11.13 (47m ago), web v2026.11.13 (1h52m ago)',
      'top error signature since first alert: upstream connect error or disconnect/reset before headers (payments-gateway)',
      'suspect: api v2026.11.13 raised the payments-gateway pool timeout from 2s to 10s',
    ],
    outputs: { alerts: 3, suspect_deploy: 'api v2026.11.13' },
    summary: '3 alerts in eu-west; errors are upstream connect failures to payments-gateway starting 4 minutes after api v2026.11.13.',
  });
  await record('incident-triage', exe, 'waiting on a person: confirm severity (team oncall)');
}

// --- Canary analysis: v3, one promoted, one failed ---------------------------------

log('runbook canary-analysis');
await publish(rb('versions/canary-analysis.v1.yaml'));
await publish(rb('versions/canary-analysis.v2.yaml'));
await publish(rb('canary-analysis.yaml'));
{
  const exe = await start('canary-analysis', { service: 'web', version: 'v2026.11.12' });
  const a = await agent(exe, 'canary-judge');
  await a.step('compare_errors', {
    logs: ['canary 5xx=0.09% baseline 5xx=0.10% over 20m'],
    verdict: 'pass',
    outputs: { canary_rate: 0.0009, baseline_rate: 0.001 },
  });
  await a.step('compare_latency', {
    logs: ['canary p95=208ms baseline p95=201ms (+3%)'],
    verdict: 'pass',
    outputs: { canary_p95_ms: 208, baseline_p95_ms: 201 },
  });
  await a.step('recommend', { outputs: { recommendation: 'promote' }, summary: 'Promote: errors and latency match the baseline.' });
  await finished(exe);
  await record('canary-analysis', exe, 'succeeded');
}
{
  const exe = await start('canary-analysis', { service: 'api', version: 'v2026.11.13' });
  const a = await agent(exe, 'canary-judge');
  await a.step('compare_errors', {
    logs: ['canary 5xx=0.31% baseline 5xx=0.12% over 20m'],
    verdict: 'warn',
    outputs: { canary_rate: 0.0031, baseline_rate: 0.0012 },
    summary: 'Canary errors 2.6x baseline but under 0.5%.',
  });
  await a.step('compare_latency', {
    logs: ['canary p95=561ms baseline p95=406ms (+38%)', 'threshold: +15%'],
    status: 'failed',
    verdict: 'fail',
    summary: 'Canary p95 561 ms is 38% above the 406 ms baseline (limit 15%).',
  });
  await finished(exe);
  await record('canary-analysis', exe, 'failed on latency');
}

// --- Database migration preflight: v2, two runs ------------------------------------

log('runbook db-migration-preflight');
await publish(rb('versions/db-migration-preflight.v1.yaml'));
await publish(rb('db-migration-preflight.yaml'));
for (const [migration, duration, lock, lockVerdict] of [
  ['2026_11_10_add_orders_region_index', 184, 120, 'pass'],
  ['2026_11_12_backfill_invoice_currency', 912, 1650, 'warn'],
]) {
  const exe = await start('db-migration-preflight', { migration });
  const a = await agent(exe, 'db-preflight');
  await a.step('lint', { logs: [`squawk ${migration}.sql: 0 errors, 0 warnings`], verdict: 'pass' });
  await a.step('dry_run', {
    logs: ['restoring snapshot 2026-11-12T02:00Z to scratch replica', `applied in ${duration}s`],
    verdict: 'pass',
    outputs: { duration_s: duration },
  });
  await a.step('locks', {
    logs: [`longest ACCESS EXCLUSIVE lock: ${lock}ms on orders`],
    verdict: lockVerdict,
    outputs: { longest_lock_ms: lock },
    summary: lockVerdict === 'warn' ? `${lock} ms lock is close to the 2 s limit; ship off-peak.` : undefined,
  });
  await finished(exe);
  await record('db-migration-preflight', exe, 'succeeded');
}

// --- Certificate sweep: v1, one run with a warning ----------------------------------

log('runbook cert-expiry-sweep');
await publish(rb('cert-expiry-sweep.yaml'));
{
  const exe = await start('cert-expiry-sweep', {});
  const a = await agent(exe, 'cert-sweeper');
  const days = [74, 74, 16, 51];
  for (const [i, d] of days.entries()) {
    await a.step(`domains[${i}].expiry`, {
      logs: [`openssl s_client: notAfter in ${d} days`],
      verdict: d < 7 ? 'fail' : d < 21 ? 'warn' : 'pass',
      outputs: { days_left: d },
    });
  }
  await finished(exe);
  await record('cert-expiry-sweep', exe, 'succeeded (one warning)');
}

// --- Cost anomaly review: v1, one run, then a draft ahead ---------------------------

log('runbook cost-anomaly-review');
await publish(rb('cost-anomaly-review.yaml'));
{
  const exe = await start('cost-anomaly-review', { week: '2026-11-02' });
  const a = await agent(exe, 'finops-agent');
  await a.step('pull_costs', { logs: ['billing export: 38 services, 4 accounts'], outputs: { total_usd: 48213.77 } });
  await a.step('anomalies', {
    logs: ['search-indexer +41% vs 4-week average: reindex job ran 3 times (cron overlap)'],
    verdict: 'warn',
    outputs: { anomalies: 1 },
    summary: 'One anomaly: search-indexer +41% from an overlapping reindex cron.',
  });
  await finished(exe);
  await record('cost-anomaly-review', exe, 'succeeded');
}
await draft(rb('drafts/cost-anomaly-review.yaml'));

// --- Never run, and a draft only -------------------------------------------------------

log('runbooks rollback-deploy, dependency-audit (never run) and queue-backlog-check (draft only)');
await publish(rb('rollback-deploy.yaml'));
await publish(rb('versions/dependency-audit.v1.yaml'));
await publish(rb('dependency-audit.yaml'));
await draft(rb('queue-backlog-check.yaml'));

// --- The browser token -------------------------------------------------------------

log("browser token (Riley's narrow personal token)");
const browser = await owner([
  'token', 'create', '--name', 'review-browser', '--expires-in', '30d',
  '--scope', 'runbooks:read', '--scope', 'runbooks:write', '--scope', 'runbooks:publish',
  '--scope', 'executions:read', '--scope', 'executions:start', '--scope', 'executions:act',
  '--scope', 'executions:admin', '--scope', 'skills:read',
]);
writeSecret('browser.token', browser.secret);
writeSecret('browser.header', `Authorization: Bearer ${browser.secret}`);

const state = {
  api_url: API,
  org_id: ORG,
  projects: { ops, sandbox, default: boot.project_id },
  users,
  teams,
  runs,
};
writeFileSync(join(STATE, 'state.json'), `${JSON.stringify(state, null, 2)}\n`, { mode: 0o600 });
for (const r of runs) log(`summary: ${r.runbook.padEnd(24)} ${r.id}  ${r.status.padEnd(9)} ${r.intended}`);
log('seed done');
