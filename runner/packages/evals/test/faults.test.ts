import { StepStopped } from '@runbook/runner-worker/loop/limits';
import { describe, expect, it } from 'vitest';
import { currentLoop, newLoop } from '../src/subject.ts';
import { finish, limits, nativeFinish, nativeRead, nativeTruncated, overload, read, readTool, reply, scripted, task, truncated } from './scripted.ts';

function run(model: ReturnType<typeof scripted>, tool = readTool(), override = {}) {
  return currentLoop({ task, model, tools: [tool], signal: new AbortController().signal, limits: { ...limits, ...override } });
}

it('ends on mid-stream overload without executing partial output', async () => {
  const result = await run(scripted(overload, reply(read)));
  expect(result.answer).toMatchObject({ reason: 'model_failed', toolCalls: 0 });
  expect(result.calls).toHaveLength(1);
});

it('rejects truncated JSON and returns validation feedback', async () => {
  const result = await run(scripted(truncated, reply(read), reply(finish)));
  expect(result.answer).toMatchObject({ status: 'submitted', toolCalls: 1 });
  expect(result.calls).toHaveLength(3);
});

it('does not replay an empty model response as an assistant message', async () => {
  const model = scripted(reply(null, 'MAX_TOKENS', '').slice(-1), reply(read), reply(finish));
  let turn = 0;
  const result = await currentLoop({ task, model, tools: [readTool()], limits,
    signal: new AbortController().signal,
    beforeCall(call) {
      if (turn++ !== 1) return;
      expect(call.prompt.some(({ role }) => role === 'assistant')).toBe(false);
      expect(JSON.stringify(call.prompt.at(-1))).toContain('invalid_response');
    },
  });
  expect(result.answer).toMatchObject({ status: 'submitted', toolCalls: 1 });
});

it('limits provider stalls', async () => {
  expect((await run(scripted('stall', 'stall', 'stall'), readTool(), { stallMs: 15 })).answer.reason).toBe('stream_stall');
});

it('ends a vanished sandbox before requesting another turn', async () => {
  const result = await run(scripted(reply(read), reply(finish)), readTool(async () => { throw new StepStopped('sandbox_gone'); }));
  expect(result.answer).toMatchObject({ reason: 'sandbox_gone', toolCalls: 1 });
  expect(result.calls).toHaveLength(1);
});

it('currently ends the step on a deadline in the MCP call', async () => {
  const result = await run(scripted(reply(read)), readTool(async () => new Promise(() => undefined)));
  expect(result.answer).toMatchObject({ reason: 'tool_timeout', toolCalls: 1 });
  expect(result.usage.toolErrors).toBe(1);
});

it.fails('future loop returns an MCP deadline as a recoverable tool result', async () => {
  let calls = 0;
  const tool = readTool(async () => ++calls === 1 ? new Promise(() => undefined) : 'recovered');
  const result = await run(scripted(reply(read), reply(read), reply(finish)), tool);
  expect(result.answer.status).toBe('submitted');
  expect(result.usage).toMatchObject({ toolErrors: 1, toolCalls: 2 });
});

it('currently treats provider refusal as invalid response and context overflow as a generic failure', async () => {
  expect((await run(scripted(reply('cannot assist', 'refusal')), readTool(), { maxTurns: 1 })).answer.reason).toBe('max_turns');
  expect((await run(scripted(new Error('context window exceeded')))).answer.reason).toBe('model_failed');
});

it('currently accepts zero-tool submissions and lets duplicate calls exhaust turns', async () => {
  expect((await run(scripted(reply(finish)))).answer)
    .toMatchObject({ status: 'submitted', toolCalls: 0 });
  const result = await run(scripted(...Array.from({ length: 6 }, () => reply(read))));
  expect(result.answer).toMatchObject({ reason: 'max_turns', toolCalls: 6 });
});

it.fails('future loop retries mid-stream overload and only runs the complete call', async () => {
  expect((await run(scripted(overload, reply(read), reply(finish)))).answer)
    .toMatchObject({ status: 'submitted', toolCalls: 1 });
});

it.fails('future loop reports a specific refusal', async () => {
  expect((await run(scripted(reply('cannot assist', 'refusal')))).answer.reason).toBe('model_refusal');
});

it.fails('future loop reports context overflow', async () => {
  expect((await run(scripted(new Error('context window exceeded')))).answer.reason).toBe('context_full');
});

it.fails('future loop stops repeated calls at five', async () => {
  const result = await run(scripted(...Array.from({ length: 6 }, () => reply(read))));
  expect(result.answer).toMatchObject({ reason: 'loop_detected', toolCalls: 5 });
});

describe('new-loop', () => {
  function runNative(model: ReturnType<typeof scripted>, tool = readTool(), override = {}) {
    return newLoop({ task, model, tools: [tool], signal: new AbortController().signal, limits: { ...limits, ...override } });
  }

  it('retries mid-stream overload and only runs the complete call', async () => {
    const result = await runNative(scripted(overload, nativeRead(), nativeFinish()));
    expect(result.answer).toMatchObject({ status: 'submitted', toolCalls: 1 });
    expect(result.calls).toHaveLength(3);
    expect(result.observed.join(' ')).not.toContain('overloaded_error');
  });

  it('returns an MCP deadline as a recoverable result', async () => {
    let calls = 0;
    const tool = readTool(async () => ++calls === 1 ? new Promise(() => undefined) : 'recovered');
    const result = await runNative(scripted(nativeRead(), nativeRead('read-2'), nativeFinish(['read-2'])), tool);
    expect(result.answer.status).toBe('submitted');
    expect(result.usage).toMatchObject({ toolErrors: 1, toolCalls: 2 });
  });

  it('reports provider refusal and context overflow specifically', async () => {
    expect((await runNative(scripted(reply('cannot assist', 'refusal')))).answer.reason).toBe('model_refusal');
    expect((await runNative(scripted(new Error('context window exceeded')))).answer.reason).toBe('context_full');
  });

  it('enters an endgame after five consecutive identical calls', async () => {
    const result = await runNative(scripted(...Array.from({ length: 6 }, (_, i) => nativeRead(`read-${i}`))));
    expect(result.answer).toMatchObject({ reason: 'loop_detected', toolCalls: 5 });
    expect(result.calls).toHaveLength(6);
  });

  it('drops a truncated tool call and retries with an instruction', async () => {
    const result = await runNative(scripted(nativeTruncated, nativeRead(), nativeFinish()));
    expect(result.answer).toMatchObject({ status: 'submitted', toolCalls: 1 });
    expect(result.observed.join(' ')).not.toContain('incomplete');
  });

  it('stops a stalled stream or a vanished sandbox', async () => {
    expect((await runNative(scripted('stall', 'stall', 'stall'), readTool(), { stallMs: 15 })).answer.reason).toBe('stream_stall');
    const missing = readTool(async () => { throw new StepStopped('sandbox_gone'); });
    const result = await runNative(scripted(nativeRead(), nativeFinish()), missing);
    expect(result.answer).toMatchObject({ reason: 'sandbox_gone', toolCalls: 1 });
    expect(result.calls).toHaveLength(1);
  });
});
