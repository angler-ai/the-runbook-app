import type { LanguageModelV4StreamPart } from '@ai-sdk/provider';
import { partStream, streamingModel } from '../src/replay.ts';
import { loopLimits } from '../src/subject.ts';
import { selectTasks } from '../src/tasks.ts';

export const task = selectTasks('logs/disk')[0]!;
export const read = { tool: 'read', arguments: { path: 'service.log' } };
export const finish = { outputs: task.expected.outputs, checklist: task.step.checklist, summary: 'The disk failure started the retry cascade.' };
export const limits = { ...loopLimits, maxTurns: 6, stallMs: 250, toolMs: 25 };

export function reply(value: unknown, raw = 'end_turn', text = JSON.stringify(value)): LanguageModelV4StreamPart[] {
  const unified = raw === 'max_tokens' ? 'length' : raw === 'refusal' ? 'content-filter' : 'stop';
  return [
    { type: 'text-start', id: 'text' }, { type: 'text-delta', id: 'text', delta: text },
    { type: 'text-end', id: 'text' },
    { type: 'finish', finishReason: { unified, raw },
      usage: { inputTokens: { total: 10, noCache: 8, cacheRead: 2, cacheWrite: 0 },
        outputTokens: { total: 5, text: 5, reasoning: 0 } } },
  ];
}

export const readTool = (call = async () => task.files['service.log']) =>
  ({ name: 'read', description: 'Read a fixture file', schema: { type: 'object' }, call });

type Script = LanguageModelV4StreamPart[] | Error | 'stall';
export function scripted(...script: Script[]) {
  return streamingModel('fixture', async () => {
    const next = script.shift();
    if (!next) throw new Error('script exhausted');
    if (next instanceof Error) throw next;
    return { stream: next === 'stall' ? new ReadableStream<LanguageModelV4StreamPart>() : partStream(next) };
  });
}

export const overload: LanguageModelV4StreamPart[] = [
  { type: 'text-start', id: 'text' }, { type: 'text-delta', id: 'text', delta: '{"tool":"read"' },
  { type: 'error', error: 'overloaded_error' },
];
export const truncated = reply(null, 'max_tokens', '{"tool":"read","arguments":');

export function nativeReply(name: string, input: unknown, id = 'tool-1'): LanguageModelV4StreamPart[] {
  return [
    { type: 'tool-input-start', id, toolName: name },
    { type: 'tool-input-delta', id, delta: JSON.stringify(input) },
    { type: 'tool-input-end', id },
    { type: 'tool-call', toolCallId: id, toolName: name, input: JSON.stringify(input) },
    { ...reply(null, 'tool_use').at(-1) as Extract<LanguageModelV4StreamPart, { type: 'finish' }>,
      finishReason: { unified: 'tool-calls', raw: 'tool_use' } },
  ];
}

export const nativeRead = (id = 'read-1') => nativeReply(read.tool, read.arguments, id);
export const nativeFinish = (ids: string[] = ['read-1']) => nativeReply('submit_result', {
  ...finish, evidence: ids.length ? [{ claim: 'Originating error', tool_call_ids: ids }] : [], unverified: [],
}, 'finish-1');
export const nativeTruncated: LanguageModelV4StreamPart[] = [
  { type: 'tool-input-start', id: 'incomplete', toolName: 'read' },
  { type: 'tool-input-delta', id: 'incomplete', delta: '{"path":"' },
  truncated.at(-1)!,
];
