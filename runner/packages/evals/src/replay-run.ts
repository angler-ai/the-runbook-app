import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { runnerSubject } from './baselines.ts';
import { hashValue, replayModel, type ModelCall } from './replay.ts';
import { selectTasks, type Answer } from './tasks.ts';
import type { Subject } from './subject.ts';

interface Recording {
  task: string;
  subject: 'current' | 'new-loop' | 'bash';
  modelId: string;
  modelProvider?: string;
  profile?: Parameters<typeof runnerSubject>[2];
  calls: ModelCall[];
  answer: Answer;
  toolNames: string[];
  toolHashes: string[];
  workspaceHash: string;
}

export async function replayTrial(recording: Recording, signal: AbortSignal, createSubject: typeof runnerSubject = runnerSubject) {
  const [task] = selectTasks(recording.task);
  if (!task || task.id !== recording.task) throw new Error('Replay requires an exact fixture task ID');
  if (recording.subject !== 'current' && recording.subject !== 'bash' && recording.subject !== 'new-loop') throw new Error('Replay requires a runner subject trace');
  const replay = replayModel(recording.modelId, recording.calls, recording.modelProvider);
  const subject: Subject = createSubject(recording.subject, () => ({ model: replay.model, cost: () => 0 }), recording.profile);
  const result = await subject.run(task, signal);
  replay.assertComplete();
  for (const name of ['answer', 'toolNames', 'toolHashes'] as const) {
    if (hashValue(result[name]) !== hashValue(recording[name])) throw new Error(`Replay divergence: ${name} changed`);
  }
  if (hashValue(result.files) !== recording.workspaceHash) throw new Error('Replay divergence: workspace changed');
}

async function main() {
  const path = process.argv[2];
  if (!path) throw new Error('Usage: npm run eval:replay -- <recorded-trial.json>');
  await replayTrial(JSON.parse(await readFile(path, 'utf8')), new AbortController().signal);
  process.stdout.write('Replay matched the recording.\n');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error: unknown) => {
    process.stderr.write(`${error instanceof Error ? error.message : 'Replay failed'}\n`);
    process.exitCode = 1;
  });
}
