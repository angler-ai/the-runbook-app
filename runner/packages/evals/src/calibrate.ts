import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { isScore } from './judge.ts';
import type { Trial } from './report.ts';

export function calibrate(trials: readonly Trial[], jsonl: string) {
  const byId = new Map(trials.map((trial) => [trial.id, trial]));
  const used = new Set<string>();
  const confusion = Array.from({ length: 4 }, () => [0, 0, 0, 0]);
  let absoluteError = 0;
  let compared = 0;
  let passAgreement = 0;
  let unavailable = 0;
  for (const line of jsonl.split('\n').filter((line) => line.trim())) {
    const label = JSON.parse(line) as { trialId?: unknown; humanScore?: unknown } | null;
    if (typeof label?.trialId !== 'string' || !isScore(label.humanScore)) throw new Error('Invalid calibration label');
    if (used.has(label.trialId)) throw new Error(`Duplicate label: ${label.trialId}`);
    used.add(label.trialId);
    const trial = byId.get(label.trialId);
    if (!trial) throw new Error(`Unknown trial: ${label.trialId}`);
    if (trial.holdout) throw new Error(`Calibration cannot use holdout trial: ${label.trialId}`);
    if (!isScore(trial.judgeScore)) { unavailable++; continue; }
    const row = confusion[label.humanScore]!;
    row[trial.judgeScore] = row[trial.judgeScore]! + 1;
    absoluteError += Math.abs(label.humanScore - trial.judgeScore);
    passAgreement += Number((label.humanScore >= 2) === (trial.judgeScore >= 2));
    compared++;
  }
  return { labels: used.size, compared, unavailable, confusion,
    exactAgreement: compared ? confusion.reduce((sum, row, i) => sum + row[i]!, 0) / compared : null,
    passAgreement: compared ? passAgreement / compared : null, meanAbsoluteError: compared ? absoluteError / compared : null };
}

async function main() {
  const [reportPath, labelPath] = process.argv.slice(2);
  if (!reportPath || !labelPath) throw new Error('Usage: npm run eval:calibrate -- <report.json> <labels.jsonl>');
  const [report, labels] = await Promise.all([readFile(reportPath, 'utf8'), readFile(labelPath, 'utf8')]);
  process.stdout.write(JSON.stringify(calibrate((JSON.parse(report) as { trials: Trial[] }).trials, labels), null, 2) + '\n');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error: unknown) => {
    process.stderr.write(`${error instanceof Error ? error.message : 'Calibration failed'}\n`);
    process.exitCode = 1;
  });
}
