import { readFileSync } from 'node:fs';
import { expect, it } from 'vitest';
import { replayCurrent, replayNew } from '../src/subject.ts';
import { limits, readTool, task } from './scripted.ts';

const fixture = (name: string): Parameters<typeof replayCurrent>[1] =>
  JSON.parse(readFileSync(new URL(`../fixtures/${name}.json`, import.meta.url), 'utf8'));
const input = () => ({ task, tools: [readTool()], limits, signal: new AbortController().signal });

it.each(['normal', 'overload', 'truncated'])('replays the %s recording against the current loop', async (name) => {
  const recording = fixture(name);
  expect((await replayCurrent(input(), recording)).answer.status).toBe(recording.answer.status);
});

it('surfaces changed tool results even when the loop catches the model error', async () => {
  await expect(replayCurrent({ ...input(), tools: [readTool(async () => 'changed')] }, fixture('normal')))
    .rejects.toThrow('Replay divergence at call 2');
});

it('rejects unused calls', async () => {
  const recording = fixture('normal');
  recording.calls.push(recording.calls[0]!);
  await expect(replayCurrent(input(), recording)).rejects.toThrow('consumed 2 of 3 calls');
});

it.each(['normal', 'overload', 'truncated'])('replays the %s recording against new-loop', async (name) => {
  const recording = fixture(`v2-${name}`);
  expect((await replayNew(input(), recording)).answer.status).toBe('submitted');
});

it('detects new-loop replay divergence when a tool response changes', async () => {
  await expect(replayNew({ ...input(), tools: [readTool(async () => 'changed')] }, fixture('v2-normal')))
    .rejects.toThrow('Replay divergence at call 2');
});
