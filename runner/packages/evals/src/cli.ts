import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { GoogleAuth } from 'google-auth-library';
import { parseArgs } from 'node:util';
import { claudeAvailable, claudeSubject, runnerSubject, subjectIds, type SubjectId } from './baselines.ts';
import { geminiAvailable, geminiSubject } from './gemini-cli.ts';
import { gradeSummary } from './judge.ts';
import { appendSpend, lockLedger, readSpendMicros } from './ledger.ts';
import { hashValue } from './replay.ts';
import { isApplicable, makeReport, markdown, type Trial } from './report.ts';
import { budgetedGateway, estimate, judgeBudget, profileFor, trialBudget } from './spend.ts';
import { grade, selectTasks } from './tasks.ts';
import type { Subject } from './subject.ts';

export function evalOptions(args: string[]) {
  const { values } = parseArgs({ args, options: {
    subject: { type: 'string', multiple: true, default: ['current'] },
    provider: { type: 'string', default: 'anthropic' },
    project: { type: 'string' },
    model: { type: 'string' },
    location: { type: 'string' },
    profile: { type: 'string', default: 'default' },
    trials: { type: 'string', default: '3' },
    tasks: { type: 'string', default: 'tuning' },
    'max-usd': { type: 'string' },
    'total-cap': { type: 'string' },
    ledger: { type: 'string' },
    'dry-run': { type: 'boolean' },
    out: { type: 'string' },
    resume: { type: 'string' },
    'record-model': { type: 'boolean' },
  } });
  const gemini = values.provider === 'vertex-gemini';
  if (values.model !== undefined && !gemini) throw new Error('--model requires --provider vertex-gemini');
  const vertex = gemini || values.provider === 'vertex';
  const applicable = subjectIds.filter((id) => id === 'claude' ? !vertex : id === 'gemini-cli' ? gemini : true);
  const selected = [...new Set(values.subject.flatMap((value) => value === 'all' ? applicable : value.split(',')))];
  if (!selected.length || !selected.every((id) => (subjectIds as readonly string[]).includes(id))) {
    throw new Error(`Unknown subject. Choose ${subjectIds.join(', ')} or all.`);
  }
  const subjects = selected as SubjectId[];
  const trials = Number(values.trials);
  if (!Number.isInteger(trials) || trials < 3 || trials > 5) throw new Error('--trials must be 3, 4 or 5');
  const tasks = selectTasks(values.tasks);
  if (!tasks.length) throw new Error('Invalid --tasks. Choose tuning, holdout, challenge, full, or a comma-separated list of unique exact task IDs.');
  if (values.provider !== 'anthropic' && !vertex) throw new Error('Choose --provider anthropic, vertex or vertex-gemini');
  const project = values.project ?? process.env.GOOGLE_VERTEX_PROJECT ?? 'angler-dev';
  const location = values.location ?? process.env.GOOGLE_VERTEX_LOCATION ?? 'global';
  if (vertex && (!/^[a-z0-9][a-z0-9-]*$/.test(project) || !/^[a-z0-9][a-z0-9-]*$/.test(location))) {
    throw new Error('Expected valid Vertex --project and --location');
  }
  if (vertex && subjects.includes('claude')) throw new Error('Claude Code requires --provider anthropic');
  if (!gemini && subjects.includes('gemini-cli')) throw new Error('Gemini CLI requires --provider vertex-gemini');
  if (gemini && !['global', 'us', 'eu'].includes(location)) throw new Error('Gemini 4 Hydrogen requires --location global, us or eu');
  const profile = profileFor(values.profile, vertex ? { project, location, gemini, model: values.model } : undefined);
  const estimates = estimate(subjects, profile, tasks, trials, values.profile, (subject, task) => isApplicable(subject, task.id));
  const total = Math.round(estimates.reduce((sum, item) => sum + item.maxUsd, 0) * 1_000_000) / 1_000_000;
  const maxUsd = Number(values['max-usd']);
  if ((!values['dry-run'] || values['max-usd'] !== undefined) && (!Number.isFinite(maxUsd) || maxUsd <= 0)) {
    throw new Error('Live evals require a positive --max-usd spend ceiling');
  }
  const totalCap = values['total-cap'] === undefined ? undefined : Number(values['total-cap']);
  if (totalCap !== undefined && (!Number.isFinite(totalCap) || totalCap <= 0 || !Number.isSafeInteger(Math.floor(totalCap * 1_000_000)))) {
    throw new Error('--total-cap requires a positive USD amount');
  }
  const ledger = resolve(values.ledger ?? fileURLToPath(new URL('../../../../eval-results/spend-ledger.jsonl', import.meta.url)));
  if (values.resume && values.out) throw new Error('Choose --resume or --out.');
  if (values['dry-run'] && !values.resume && maxUsd < total) throw new Error(`Estimated $${total.toFixed(6)} exceeds --max-usd $${maxUsd.toFixed(6)}`);
  return { subjects, profile, vertex: vertex ? { project, location } : undefined, profileName: values.profile, trials, tasks, estimates, total,
    maxUsd, totalCap, ledger, resume: values.resume, out: values.out, dryRun: values['dry-run'], record: values['record-model'] };
}

interface Dependencies {
  hasClaude: typeof claudeAvailable;
  hasGemini: typeof geminiAvailable;
  key(): string | undefined;
  adc(): Promise<string>;
  gateway: typeof budgetedGateway;
  createRunner: typeof runnerSubject;
  createClaude: typeof claudeSubject;
  createGemini: typeof geminiSubject;
  print(value: string): void;
}

const google = new GoogleAuth({ scopes: 'https://www.googleapis.com/auth/cloud-platform' });
const defaults: Dependencies = {
  async adc() {
    const token = await google.getAccessToken();
    if (!token) throw new Error('Google Application Default Credentials did not return an access token');
    return token;
  },
  hasClaude: claudeAvailable, hasGemini: geminiAvailable, key: () => process.env.ANTHROPIC_API_KEY, gateway: budgetedGateway,
  createRunner: runnerSubject, createClaude: claudeSubject, createGemini: geminiSubject, print: (value) => process.stdout.write(value + '\n'),
};

export async function main(args: string[], dependencies: Partial<Dependencies> = {}, signal: AbortSignal = new AbortController().signal) {
  const options = evalOptions(args);
  const deps = { ...defaults, ...dependencies };
  const prior = options.resume ? JSON.parse(await readFile(options.resume, 'utf8')) as {
    trials: Trial[]; skips: string[]; target?: object;
  } : undefined;
  const target = { provider: options.profile.provider, model: options.profile.model, ...options.vertex };
  if (prior?.target && hashValue(prior.target) !== hashValue(target)) throw new Error('Resume model target differs from the saved report.');
  const trials = prior?.trials ?? [];
  const completed = new Set(trials.map(({ id }) => id));
  if (completed.size !== trials.length || trials.some(({ usage, judgeCostUsd }) =>
    !Number.isFinite(usage.costUsd) || usage.costUsd < 0 || !Number.isFinite(judgeCostUsd) || judgeCostUsd < 0)) {
    throw new Error('Resume report contains duplicate trials or invalid spend.');
  }
  const idFor = (subject: string, task: string, trial: number) => `${options.profileName}/${subject}/${task}/${trial}`;
  const pending = (subject: string) => options.tasks.some(({ id }) => isApplicable(subject, id) &&
    Array.from({ length: options.trials }, (_, i) => i + 1).some((trial) => !completed.has(idFor(subject, id, trial))));
  let spent = trials.reduce((sum, row) => sum + row.usage.costUsd + row.judgeCostUsd, 0);
  const remaining = () => Math.floor((options.maxUsd - spent) * 1_000_000 + 1e-7) / 1_000_000;
  const estimates = prior ? estimate(options.subjects, options.profile, options.tasks, options.trials, options.profileName,
    (subject, task, trial) => isApplicable(subject, task.id) && !completed.has(idFor(subject, task.id, trial))) : options.estimates;
  const totalUsd = Math.round(estimates.reduce((sum, row) => sum + row.maxUsd, 0) * 1_000_000) / 1_000_000;
  deps.print(JSON.stringify({ profile: options.profileName, provider: options.profile.provider, model: options.profile.model,
    ...options.vertex, estimates, totalUsd, priorSpendUsd: spent }, null, 2));
  if (options.dryRun) return;
  const skips = [...(prior?.skips ?? [])];
  const subjects = options.subjects.filter(pending);
  if (subjects.includes('claude') && !await deps.hasClaude()) {
    skips.push('Claude Code skipped because the claude CLI is unavailable.');
    subjects.splice(subjects.indexOf('claude'), 1);
    deps.print(skips.at(-1)!);
  }
  if (subjects.includes('gemini-cli') && !await deps.hasGemini(signal)) {
    const detail = 'Gemini CLI skipped because npx could not run @google/gemini-cli@0.70.0.';
    skips.push(detail);
    subjects.splice(subjects.indexOf('gemini-cli'), 1);
    deps.print(detail);
  }
  const profile = { provider: options.profile.provider, model: options.profile.model, name: options.profileName,
    maxOutputTokens: options.profile.maxOutputTokens };
  const key = subjects.length ? options.vertex ? deps.adc : deps.key() : undefined;
  if (subjects.length && options.vertex) {
    try { await deps.adc(); }
    catch { throw new Error('Vertex evals require Google Application Default Credentials'); }
  }
  if (subjects.length && !key) throw new Error('Set ANTHROPIC_API_KEY to run live evals');
  const out = options.resume ? dirname(options.resume) : options.out ?? join('eval-results', new Date().toISOString().replaceAll(':', '-'));
  await mkdir(out, { recursive: true });
  const notApplicable = options.subjects.flatMap((subject) => options.tasks
    .filter(({ id }) => !isApplicable(subject, id))
    .map(({ id }) => ({ subject, task: id, reason: 'Restricted host CLI has no shell tool' })));
  const save = async () => {
    const report = { ...makeReport(trials, skips, notApplicable), target };
    for (const [name, text] of [[options.resume ? basename(options.resume) : 'report.json', JSON.stringify(report, null, 2) + '\n'], ['report.md', markdown(report)]] as const) {
      await writeFile(join(out, name + '.tmp'), text, { mode: 0o600 });
      await rename(join(out, name + '.tmp'), join(out, name));
    }
  };
  const capMicros = options.totalCap === undefined ? undefined : Math.floor(options.totalCap * 1_000_000 + 1e-8);
  // Interleave subjects for each task/trial. Each adapter provisions a fresh
  // workspace and gets an isolated subject budget and a separate judge cap.
  try {
    for (const task of options.tasks) {
      for (let trial = 1; trial <= options.trials; trial++) {
        for (const subject of subjects) {
          signal.throwIfAborted();
          const id = idFor(subject, task.id, trial);
          if (!isApplicable(subject, task.id) || completed.has(id)) continue;
          const allocation = trialBudget(subject, options.profile, task);
          const external = subject === 'claude';
          if (remaining() <= 0 || (external && remaining() < allocation)) {
            skips.push(`Global budget stopped before ${id}.`);
            return;
          }
          const budget = Math.min(remaining(), allocation);
          let result: Awaited<ReturnType<Subject['run']>>;
          let wrongModel: boolean;
          let denied: boolean | undefined;
          let judgeCostUsd: number;
          // Reserve the trial's full allocation in the shared ledger under a short lock, run the trial
          // unlocked, then settle the actual spend. Parallel runs then share one cumulative cap.
          const nextMicros = Math.round((allocation + judgeBudget(options.profile)) * 1_000_000);
          const reservationId = `${resolve(out)}#${id}#${process.pid}#${Date.now()}`;
          const reserveUnlock = await lockLedger(options.ledger);
          try {
            if (capMicros !== undefined && (await readSpendMicros(options.ledger)) + nextMicros > capMicros) {
              const detail = `Cumulative spend cap stopped before ${id}.`;
              skips.push(detail);
              deps.print(detail);
              return;
            }
            await appendSpend(options.ledger, { timestamp: new Date().toISOString(), subject, task: task.id, runDir: resolve(out),
              trialId: id, modelCostUsd: nextMicros / 1_000_000, judgeCostUsd: 0, reservation: true, reservationId });
          } finally {
            await reserveUnlock();
          }
          {
            let modelBudget: ReturnType<typeof budgetedGateway> | undefined;
            const adapter: Subject = subject === 'claude'
              ? deps.createClaude(options.profile.model, () => budget)
              : subject === 'gemini-cli' ? deps.createGemini(options.profile, options.vertex!, () => budget)
              : deps.createRunner(subject, () => modelBudget = deps.gateway(options.profileName, options.profile, key!, budget, undefined, subject === 'new-loop' || (subject === 'bash' && profile.provider === 'vertex-gemini')), profile);
            result = await adapter.run(task, signal);
            spent += result.usage.costUsd;
            wrongModel = result.answer.reason === 'unexpected_gemini_model';
            denied = result.answer.status === 'failed' && (modelBudget?.denied() || result.answer.reason?.startsWith('limit:') ||
              ['error_max_budget_usd', 'budget_exceeded'].includes(result.answer.reason ?? ''));
            const original = result.answer;
            if (result.usage.costUsd > budget + 1e-8 || denied) {
              result.answer = { status: 'failed', reason: 'budget_exceeded', toolCalls: result.usage.toolCalls };
            }
            const score = grade(task, result.answer, result.files, [
              ...result.observed, ...(original === result.answer ? [] : [JSON.stringify(original)]),
            ], result.toolNames);
            const judgeCap = Math.min(remaining(), judgeBudget(options.profile));
            const judgeEligible = !wrongModel && score.checks.noLeaks && result.answer.status !== 'failed';
            const judge = judgeEligible && judgeCap > 0
              ? deps.gateway(options.profileName, options.profile, key!, judgeCap) : undefined;
            const judgeScore = judge ? await gradeSummary(judge.model, task, result.answer, signal) : null;
            judgeCostUsd = judge?.cost() ?? 0;
            const judgeReason = judgeEligible && (judgeCap <= 0 || judge?.denied() || judgeCostUsd > judgeCap + 1e-8)
              ? 'budget_exceeded' as const : undefined;
            spent += judgeCostUsd;
            trials.push({ id, task: task.id, category: task.category, difficulty: task.difficulty, holdout: task.holdout, trial, profile: options.profileName,
              subject, answer: score.checks.noLeaks ? result.answer : { status: result.answer.status, reason: 'secret_leak' },
              grade: score, usage: result.usage, judgeScore: judgeReason ? null : judgeScore, judgeCostUsd, ...(judgeReason && { judgeReason }) });
          }
          // A trial that throws leaves its reservation open, so the ledger keeps charging its full allocation.
          const settleUnlock = await lockLedger(options.ledger);
          try {
            await appendSpend(options.ledger, { timestamp: new Date().toISOString(), subject, task: task.id,
              runDir: resolve(out), trialId: id, modelCostUsd: result.usage.costUsd, judgeCostUsd, reservationId });
          } finally {
            await settleUnlock();
          }
          // Save after every trial so an interrupted run loses at most one trial and can be resumed.
          await save();
          deps.print(`[${trials.length}] ${id} ${result.answer.status}${result.answer.reason ? `/${result.answer.reason}` : ''} ` +
            `$${(result.usage.costUsd + judgeCostUsd).toFixed(4)} (spent $${spent.toFixed(4)})`);
          if (wrongModel) {
            const detail = `Gemini CLI reported a model other than ${options.profile.model}. Stopping subsequent calls.`;
            skips.push(detail);
            throw new Error(detail);
          }
          if (spent > options.maxUsd + 1e-8 || (denied && budget < allocation && result.answer.reason === 'budget_exceeded')) {
            skips.push(`Global budget stopped after ${id}.`);
            return;
          }
          if (options.record && result.calls) {
            const trace = { task: task.id, subject, modelId: result.modelId ?? options.profileName, modelProvider: result.modelProvider, profile, calls: result.calls,
              answer: result.answer, toolNames: result.toolNames, toolHashes: result.toolHashes, workspaceHash: hashValue(result.files) };
            const filename = `${id.replaceAll('/', '-')}.json`;
            await writeFile(join(out, basename(filename)), JSON.stringify(trace, null, 2) + '\n', { mode: 0o600, flag: 'wx' });
          }
        }
      }
    }
  } finally {
    await save();
    deps.print(`Reports written to ${out}`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const stop = new AbortController();
  process.once('SIGINT', () => stop.abort(new Error('Eval interrupted')));
  process.once('SIGTERM', () => stop.abort(new Error('Eval interrupted')));
  main(process.argv.slice(2), {}, stop.signal).catch((error: unknown) => {
    process.stderr.write(`${error instanceof Error ? error.message : 'Evaluation failed'}\n`);
    process.exitCode = 1;
  });
}
