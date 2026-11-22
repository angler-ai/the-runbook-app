import { readFileSync } from 'node:fs';
import { expect, it } from 'vitest';
import { geminiThinking } from '@runbook/runner-shared/gemini-settings';
import { replayModel, type ModelCall } from '../src/replay.ts';
import { makeReport } from '../src/report.ts';
import { grade } from '../src/tasks.ts';
import { newLoop } from '../src/subject.ts';
import { limits, nativeReply, readTool, reply, scripted, task } from './scripted.ts';

it('replays parallel Gemini calls with a signed first part and ordered results', async () => {
  const calls = JSON.parse(readFileSync(new URL('../fixtures/v2-gemini.json', import.meta.url), 'utf8')) as ModelCall[];
  const replay = replayModel('gemini-4-hydrogen', calls, 'google.vertex.chat');
  let turn = 0;
  const result = await newLoop({ task, model: replay.model, tools: [{ ...readTool(), parallel: true }], limits,
    signal: new AbortController().signal, gemini: geminiThinking('default'),
    beforeCall(call) {
      if (turn++ === 0) return;
      expect(call.prompt.at(-2)).toMatchObject({ role: 'assistant', content: [
        { toolCallId: 'call-a', providerOptions: { googleVertex: { thoughtSignature: 'opaque+/A=' } } },
        { toolCallId: 'call-b' },
      ] });
      expect(call.prompt.at(-1)).toMatchObject({ role: 'tool', content: [{ toolCallId: 'call-a' }, { toolCallId: 'call-b' }] });
    },
  });
  replay.assertComplete();
  expect(result.answer).toMatchObject({ status: 'submitted', outputs: task.expected.outputs });
  expect(result.toolNames).toEqual(['read', 'read']);
  expect(result.usage).toMatchObject({ inputTokens: 20, cacheReadTokens: 4 });
  const report = makeReport([{ id: 'fixture', task: task.id, category: task.category, holdout: task.holdout,
    trial: 1, profile: 'default', subject: 'new-loop', answer: result.answer,
    grade: grade(task, result.answer, task.files, result.observed, result.toolNames),
    usage: result.usage, judgeScore: null, judgeCostUsd: 0 }]);
  expect(report.subjects[0]).toMatchObject({ cacheHitRate: 0.2 });
});

it('uses Gemini raw MAX_TOKENS to request a bounded finish-only endgame', async () => {
  const maxed = reply(null, 'MAX_TOKENS', 'Partial output.');
  const finish = nativeReply('report_blocked', {
    kind: 'missing_information', reason: 'Missing fixture fields.', details: 'The log lacks a timestamp.',
  });
  const model = scripted(maxed, finish);
  let turn = 0;
  const result = await newLoop({ task, model, tools: [readTool()], limits: { ...limits, maxTurns: 1 },
    signal: new AbortController().signal, gemini: geminiThinking('default'),
    beforeCall(call) {
      if (turn++ === 0) return;
      expect(call.toolChoice).toEqual({ type: 'required' });
      expect(call.tools?.map((tool) => tool.type === 'function' && tool.name)).toEqual(['submit_result', 'report_blocked']);
      expect(call.prompt.at(-1)).toMatchObject({ role: 'user', content: [
        { type: 'text', text: expect.stringContaining('This is your final turn') },
      ] });
      expect(call.providerOptions).toEqual({ googleVertex: { thinkingConfig: { thinkingLevel: 'low' } } });
    },
  });
  expect(result.answer).toMatchObject({ status: 'blocked', kind: 'missing_information' });
});
