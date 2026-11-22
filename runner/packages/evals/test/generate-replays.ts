import { writeFile } from 'node:fs/promises';
import { currentLoop, newLoop } from '../src/subject.ts';
import { finish, limits, nativeFinish, nativeRead, nativeReply, nativeTruncated, overload, read, readTool, reply, scripted, task, truncated } from './scripted.ts';

for (const [name, script] of Object.entries({
  normal: [reply(read), reply(finish)],
  overload: [overload],
  truncated: [truncated, reply(read), reply(finish)],
})) {
  const model = scripted(...script);
  const { calls, answer, toolNames, toolHashes } = await currentLoop({
    task, model, tools: [readTool()], limits, signal: new AbortController().signal,
  });
  await writeFile(new URL(`../fixtures/${name}.json`, import.meta.url),
    JSON.stringify({ modelId: model.modelId, calls, answer, toolNames, toolHashes }, null, 2) + '\n');
}

for (const [name, script] of Object.entries({
  normal: [nativeRead(), nativeFinish()],
  overload: [overload, nativeRead(), nativeFinish()],
  truncated: [nativeTruncated, nativeRead(), nativeFinish()],
})) {
  const model = scripted(...script);
  const { calls, answer, toolNames, toolHashes } = await newLoop({
    task, model, tools: [readTool()], limits, signal: new AbortController().signal,
  });
  await writeFile(new URL(`../fixtures/v2-${name}.json`, import.meta.url),
    JSON.stringify({ modelId: model.modelId, calls, answer, toolNames, toolHashes }, null, 2) + '\n');
}

const geminiFirst = nativeReply('read', read.arguments, 'call-a');
const geminiSecond = nativeReply('read', read.arguments, 'call-b');
const geminiEnd = geminiFirst.pop()!;
if (geminiEnd.type !== 'finish' || geminiFirst[3]?.type !== 'tool-call') throw new Error('Unexpected fake stream');
geminiEnd.finishReason.raw = 'STOP';
geminiFirst[3].providerMetadata = {
  googleVertex: { thoughtSignature: 'opaque+/A=' }, vertex: { thoughtSignature: 'opaque+/A=' },
};
const geminiFinal = nativeReply('submit_result', { ...finish, outputs: JSON.stringify(finish.outputs),
  evidence: [{ claim: 'Originating error', tool_call_ids: ['call-a', 'call-b'] }], unverified: [] }, 'finish-1');
const geminiFinalEnd = geminiFinal.at(-1)!;
if (geminiFinalEnd.type === 'finish') geminiFinalEnd.finishReason.raw = 'STOP';
const geminiModel = { ...scripted([...geminiFirst, ...geminiSecond.slice(0, -1), geminiEnd], geminiFinal),
  modelId: 'gemini-4-hydrogen', provider: 'google.vertex.chat' };
const geminiResult = await newLoop({ task, model: geminiModel, tools: [{ ...readTool(), parallel: true }], limits,
  signal: new AbortController().signal });
if (geminiResult.answer.status !== 'submitted') throw new Error(JSON.stringify(geminiResult.answer));
await writeFile(new URL('../fixtures/v2-gemini.json', import.meta.url), JSON.stringify(geminiResult.calls, null, 2) + '\n');
