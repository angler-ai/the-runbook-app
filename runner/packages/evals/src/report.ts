import { emptyUsage, type Usage } from './subject.ts';
import type { Answer, grade, Task } from './tasks.ts';
import type { RubricScore } from './judge.ts';

export interface Trial {
  id: string;
  task: string;
  category: string;
  difficulty?: Task['difficulty'];
  holdout: boolean;
  trial: number;
  subject: string;
  profile: string;
  answer: Answer;
  grade: ReturnType<typeof grade>;
  usage: Usage;
  judgeScore: RubricScore | null;
  judgeCostUsd: number;
  judgeReason?: 'budget_exceeded';
}

const mean = (values: readonly number[]) => values.reduce((sum, value) => sum + value, 0) / values.length;
const ratio = (a: number, b: number) => b === 0 ? null : a / b;

export function summarize(trials: readonly Trial[]) {
  const total = emptyUsage();
  for (const { usage } of trials) {
    for (const key of Object.keys(total) as (keyof Usage)[]) total[key] += usage[key];
  }
  const success = trials.filter((row) => row.grade.success).length;
  const submitted = trials.filter((row) => row.answer.status === 'submitted').length;
  const zeroToolSubmissions = trials.filter((row) => row.answer.status === 'submitted' && row.usage.toolCalls === 0).length;
  const falseSuccess = trials.filter((row) => row.grade.falseSuccess).length;
  const falseBlocked = trials.filter((row) => row.grade.falseBlocked).length;
  const judgeScores = trials.flatMap((row) => row.judgeScore === null ? [] : [row.judgeScore]);
  return { trials: trials.length, success, submitted, zeroToolSubmissions, falseSuccess, falseBlocked,
    successRate: ratio(success, trials.length), falseSuccessRate: ratio(falseSuccess, trials.length),
    falseBlockedRate: ratio(falseBlocked, trials.length), costPerSuccess: ratio(total.costUsd, success),
    meanTurns: ratio(total.turns, trials.length), cacheHitRate: ratio(total.cacheReadTokens, total.inputTokens),
    toolErrorRate: ratio(total.toolErrors, total.toolCalls), ...total,
    judged: judgeScores.length, meanJudgeScore: judgeScores.length ? mean(judgeScores) : null,
    judgeCostUsd: trials.reduce((sum, row) => sum + row.judgeCostUsd, 0) };
}

function grouped(trials: readonly Trial[], keys: (row: Trial) => string[]) {
  const groups = new Map<string, Trial[]>();
  for (const row of trials) {
    const key = JSON.stringify(keys(row));
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key)!.push(row);
  }
  return [...groups.entries()];
}

function pairedComparison(left: Trial[], right: Trial[]) {
  const rightById = new Map(right.map((trial) => [`${trial.task}/${trial.trial}`, trial]));
  const paired = left.flatMap((trial) => {
    const other = rightById.get(`${trial.task}/${trial.trial}`);
    return other ? [[trial, other] as const] : [];
  });
  const tasks = new Map<string, (typeof paired)[number][]>();
  for (const pair of paired) {
    if (!tasks.has(pair[0].task)) tasks.set(pair[0].task, []);
    tasks.get(pair[0].task)!.push(pair);
  }
  if (!paired.length) return undefined;
  const groups = [...tasks.values()];
  let seed = 20261120;
  const random = (n: number) => {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
    return Math.floor(seed / 2 ** 32 * n);
  };
  const samples = Array.from({ length: 2000 }, () => groups.flatMap(() => groups[random(groups.length)]!));
  function bounds(metric: (sample: typeof paired) => number | null) {
    const values = samples.map(metric).filter((value) => value !== null).sort((a, b) => a - b);
    return { delta: metric(paired), low: values[Math.floor(values.length * 0.025)] ?? null,
      high: values[Math.floor(values.length * 0.975)] ?? null, definedBootstrapSamples: values.length };
  }
  function costPerSuccess(rows: Trial[]) {
    return ratio(rows.reduce((sum, row) => sum + row.usage.costUsd, 0), rows.filter((row) => row.grade.success).length);
  }
  return {
    profile: left[0]!.profile, left: left[0]!.subject, right: right[0]!.subject, pairs: paired.length, tasks: groups.length,
    successRate: bounds((sample) => mean(sample.map(([a, b]) => Number(a.grade.success) - Number(b.grade.success)))),
    falseSuccessRate: bounds((sample) => mean(sample.map(([a, b]) => Number(a.grade.falseSuccess) - Number(b.grade.falseSuccess)))),
    falseBlockedRate: bounds((sample) => mean(sample.map(([a, b]) => Number(a.grade.falseBlocked) - Number(b.grade.falseBlocked)))),
    costUsd: bounds((sample) => mean(sample.map(([a, b]) => a.usage.costUsd - b.usage.costUsd))),
    costPerSuccess: bounds((sample) => {
      const a = costPerSuccess(sample.map(([row]) => row));
      const b = costPerSuccess(sample.map(([, row]) => row));
      return a === null || b === null ? null : a - b;
    }),
  };
}

export function requiresShell(task: string) {
  return /^(tested-edit|background)\//.test(task);
}

export function isApplicable(subject: string, task: string) {
  return !['gemini-cli', 'claude'].includes(subject) || !requiresShell(task);
}

export function makeReport(trials: readonly Trial[], skips: readonly string[] = [],
  notApplicable: readonly { subject: string; task: string; reason: string }[] = []) {
  const applicable = trials.filter(({ subject, task }) => isApplicable(subject, task));
  const excluded = trials.filter(({ subject, task }) => !isApplicable(subject, task));
  const subjectGroups = grouped(applicable, (row) => [row.profile, row.subject]);
  const taskGroups = grouped(applicable, (row) => [row.profile, row.subject, row.task]);
  const withLabels = ([keys, rows]: [string, Trial[]]) => ({
    profile: rows[0]!.profile, subject: rows[0]!.subject,
    ...(JSON.parse(keys).length === 3 && { task: rows[0]!.task }), ...summarize(rows),
  });
  const comparisons = subjectGroups.flatMap(([, left], i) => subjectGroups.slice(i + 1).flatMap(([, right]) => {
    if (left[0]!.profile !== right[0]!.profile) return [];
    const comparison = pairedComparison(left, right);
    return comparison ? [comparison] : [];
  }));
  const subjects = subjectGroups.map(withLabels);
  const ceiling = subjects.filter(({ successRate }) => successRate !== null && successRate >= 0.98)
    .map(({ subject, profile, trials, successRate }) => ({ subject, profile, trials, successRate }));
  return { trials, skips, notApplicable, excluded, ceiling, subjects, tasks: taskGroups.map(withLabels), comparisons };
}

const fixed = (value: number | null) => value === null ? '—' : value.toFixed(3);
const row = (cells: (string | number)[]) => `| ${cells.join(' | ')} |`;

export function markdown(report: ReturnType<typeof makeReport>) {
  const header = ['Profile', 'Subject', 'Task', 'Trials', 'Success', 'False success', 'False blocked',
    'Turns', 'Input / output tokens', 'Cost USD', 'Cost per success', 'Cache hits', 'Tool errors', 'Mean rubric score'];
  const stats = (rows: typeof report.subjects) => [row(header), row(header.map(() => '---')), ...rows.map((item) =>
    row([item.profile, item.subject, item.task ?? 'All', item.trials, fixed(item.successRate), fixed(item.falseSuccessRate),
      fixed(item.falseBlockedRate), fixed(item.meanTurns), `${item.inputTokens} / ${item.outputTokens}`, fixed(item.costUsd),
      fixed(item.costPerSuccess), fixed(item.cacheHitRate), fixed(item.toolErrorRate), fixed(item.meanJudgeScore)]))];
  const interval = ({ delta, low, high }: { delta: number | null; low: number | null; high: number | null }) =>
    `${fixed(delta)} [${fixed(low)}, ${fixed(high)}]`;
  const comparisonHeader = ['Profile', 'Left − right', 'Pairs', 'Success', 'False success', 'False blocked', 'Cost USD', 'Cost per success'];
  return ['# Runner evaluation', '', ...report.skips.map((skip) => `- ${skip}`),
    ...report.notApplicable.map(({ subject, task, reason }) => `- Not applicable: ${subject} / ${task} (${reason})`),
    ...(report.excluded.length ? [`- Excluded ${report.excluded.length} historical trials requiring an unavailable shell tool.`] : []),
    ...report.ceiling.map(({ profile, subject, trials, successRate }) =>
      `- Ceiling warning: ${profile} / ${subject} reached ${(successRate! * 100).toFixed(1)}% success across ${trials} applicable trials.`),
    '', '## By subject', '', ...stats(report.subjects), '', '## By task', '', ...stats(report.tasks),
    '', '## Paired comparisons', '', '95% percentile intervals resample whole tasks and retain paired trials.',
    '', row(comparisonHeader), row(comparisonHeader.map(() => '---')), ...report.comparisons.map((item) =>
      row([item.profile, `${item.left} − ${item.right}`, item.pairs, interval(item.successRate), interval(item.falseSuccessRate),
        interval(item.falseBlockedRate), interval(item.costUsd), interval(item.costPerSuccess)])), ''].join('\n');
}
