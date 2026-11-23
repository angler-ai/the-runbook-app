// Live decision evals (plan R1-D3): runs each decision task through the
// worker's decision loop on a real model, grades it deterministically, and
// charges the shared spend ledger under the same reserve-then-settle lock as
// the main eval CLI.
import { mkdir, rename, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { GoogleAuth } from 'google-auth-library';
import { decisionTasks, gradeDecision, runDecisionEval } from './decisions.ts';
import { appendSpend, lockLedger, readSpendMicros } from './ledger.ts';
import { budgetedGateway, profileFor } from './spend.ts';

export interface DecisionTrial {
  id: string;
  task: string;
  trial: number;
  option: string | null;
  abstained: boolean;
  errors: string[];
  costUsd: number;
  failure?: string;
  reason?: string;
  tools?: string[];
}

export function decisionOptions(args: string[]) {
  const { values } = parseArgs({ args, options: {
    provider: { type: 'string', default: 'anthropic' },
    profile: { type: 'string', default: 'default' },
    project: { type: 'string' },
    location: { type: 'string' },
    trials: { type: 'string', default: '3' },
    'trial-usd': { type: 'string', default: '0.50' },
    'max-usd': { type: 'string' },
    'total-cap': { type: 'string' },
    ledger: { type: 'string' },
    out: { type: 'string' },
  } });
  const gemini = values.provider === 'vertex-gemini';
  if (!gemini && values.provider !== 'anthropic') throw new Error('Choose --provider anthropic or vertex-gemini');
  const trials = Number(values.trials);
  if (!Number.isInteger(trials) || trials < 1 || trials > 5) throw new Error('--trials must be 1 to 5');
  const trialUsd = Number(values['trial-usd']);
  const maxUsd = Number(values['max-usd']);
  if (!(trialUsd > 0) || !(maxUsd > 0)) throw new Error('Live decision evals require positive --trial-usd and --max-usd');
  const totalCap = values['total-cap'] === undefined ? undefined : Number(values['total-cap']);
  if (totalCap !== undefined && !(totalCap > 0)) throw new Error('--total-cap requires a positive USD amount');
  const project = values.project ?? process.env.GOOGLE_VERTEX_PROJECT ?? 'angler-dev';
  const location = values.location ?? process.env.GOOGLE_VERTEX_LOCATION ?? 'global';
  const profile = profileFor(values.profile, gemini ? { project, location, gemini } : undefined);
  const ledger = resolve(values.ledger ?? fileURLToPath(new URL('../../../../eval-results/spend-ledger.jsonl', import.meta.url)));
  const out = resolve(values.out ?? fileURLToPath(new URL(`../../../../eval-results/decisions-${profile.provider}-${Date.now()}`, import.meta.url)));
  return { gemini, profileName: values.profile, profile, trials, trialUsd, maxUsd, totalCap, ledger, out };
}

export function summarize(trials: readonly DecisionTrial[]) {
  const byTask = new Map<string, { passed: number; total: number }>();
  for (const { task, errors } of trials) {
    const row = byTask.get(task) ?? { passed: 0, total: 0 };
    row.total += 1;
    if (!errors.length) row.passed += 1;
    byTask.set(task, row);
  }
  const passed = trials.filter(({ errors }) => !errors.length).length;
  const costUsd = trials.reduce((sum, { costUsd }) => sum + costUsd, 0);
  return { passed, total: trials.length, costUsd: Math.round(costUsd * 1_000_000) / 1_000_000,
    tasks: Object.fromEntries(byTask) };
}

export async function main(args: string[], signal: AbortSignal = new AbortController().signal) {
  const options = decisionOptions(args);
  const credential: string | (() => Promise<string>) = options.gemini ? await (async () => {
    const google = new GoogleAuth({ scopes: 'https://www.googleapis.com/auth/cloud-platform' });
    return async () => {
      const token = await google.getAccessToken();
      if (!token) throw new Error('Google Application Default Credentials did not return an access token');
      return token;
    };
  })() : (() => {
    const key = process.env.ANTHROPIC_API_KEY;
    if (!key) throw new Error('ANTHROPIC_API_KEY is required for --provider anthropic');
    return key;
  })();
  await mkdir(options.out, { recursive: true, mode: 0o700 });
  const trials: DecisionTrial[] = [];
  const target = { provider: options.profile.provider, model: options.profile.model };
  const save = async () => {
    const report = { target, summary: summarize(trials), trials };
    await writeFile(join(options.out, 'report.json.tmp'), JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
    await rename(join(options.out, 'report.json.tmp'), join(options.out, 'report.json'));
  };
  const capMicros = options.totalCap === undefined ? undefined : Math.floor(options.totalCap * 1_000_000 + 1e-8);
  let spent = 0;
  for (const task of decisionTasks) {
    for (let trial = 1; trial <= options.trials; trial++) {
      signal.throwIfAborted();
      const id = `${options.profileName}/decision-loop/${task.id}/${trial}`;
      const budget = Math.min(options.trialUsd, options.maxUsd - spent);
      if (budget < options.trialUsd) {
        process.stdout.write(`Run budget stopped before ${id}.\n`);
        await save();
        return summarize(trials);
      }
      const reservationId = `${options.out}#${id}#${process.pid}#${Date.now()}`;
      const unlock = await lockLedger(options.ledger);
      try {
        if (capMicros !== undefined && (await readSpendMicros(options.ledger)) + Math.round(budget * 1_000_000) > capMicros) {
          process.stdout.write(`Cumulative spend cap stopped before ${id}.\n`);
          await save();
          return summarize(trials);
        }
        await appendSpend(options.ledger, { timestamp: new Date().toISOString(), subject: 'decision-loop', task: task.id,
          runDir: options.out, trialId: id, modelCostUsd: budget, judgeCostUsd: 0, reservation: true, reservationId });
      } finally {
        await unlock();
      }
      const gateway = budgetedGateway(options.profileName, options.profile, credential, budget, undefined, true);
      let entry: DecisionTrial;
      try {
        const result = await runDecisionEval(task, gateway.model, signal);
        const errors = gradeDecision(task, result);
        const finish = result.finish;
        entry = { id, task: task.id, trial, option: finish && 'option' in finish ? finish.option : null,
          abstained: !!finish && 'reason' in finish, errors, costUsd: gateway.cost(),
          ...(finish && 'reason' in finish && { reason: finish.reason.slice(0, 300) }), tools: result.reads.map(({ tool }) => tool), ...(gateway.denied() && { failure: 'budget_denied' }) };
      } catch (error) {
        entry = { id, task: task.id, trial, option: null, abstained: false, errors: ['run_failed'], costUsd: gateway.cost(),
          failure: gateway.denied() ? 'budget_exceeded' : error instanceof Error ? error.message.slice(0, 200) : 'unknown' };
      }
      spent += entry.costUsd;
      trials.push(entry);
      const settle = await lockLedger(options.ledger);
      try {
        await appendSpend(options.ledger, { timestamp: new Date().toISOString(), subject: 'decision-loop', task: task.id,
          runDir: options.out, trialId: id, modelCostUsd: entry.costUsd, judgeCostUsd: 0, reservationId });
      } finally {
        await settle();
      }
      await save();
      process.stdout.write(`[${trials.length}] ${id} ${entry.errors.length ? `FAIL ${entry.errors.join(',')}` : 'pass'} ` +
        `${entry.option ?? (entry.abstained ? 'abstained' : '-')} $${entry.costUsd.toFixed(4)} (spent $${spent.toFixed(4)})\n`);
    }
  }
  const summary = summarize(trials);
  process.stdout.write(JSON.stringify({ target, out: options.out, ...summary }) + '\n');
  return summary;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const stop = new AbortController();
  process.once('SIGINT', () => stop.abort());
  main(process.argv.slice(2), stop.signal).catch((error: unknown) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}
