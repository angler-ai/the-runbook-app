import { streamText, type LanguageModel, type ModelMessage } from 'ai';
import { Ajv2020 } from 'ajv/dist/2020.js';
import { createRequire } from 'node:module';
import type { FormatsPlugin } from 'ajv-formats';
import type { Outcome } from '../claim.ts';
import { whileActive } from '../lease.ts';
import { deadline, type LoopLimits, StepStopped } from './limits.ts';
import { toolPrompt } from './prompt.ts';
import type { AgentTool } from './tools.ts';

export interface AgentLoop {
  model: LanguageModel;
  system: string;
  input: string;
  outputSchema: unknown;
  checklist?: readonly string[];
  check?: boolean;
  record?(role: 'model' | 'tool', value: unknown): Promise<unknown>;
  tools: readonly AgentTool[];
  limits: LoopLimits;
  signal: AbortSignal;
}
export type AgentResult = Outcome & { completed?: string[] };

const addFormats = createRequire(import.meta.url)('ajv-formats') as FormatsPlugin;
const isObject = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === 'object' && !Array.isArray(value);

export function compileOutputSchema(schema: unknown) {
  const nodes = [schema];
  while (nodes.length) {
    const node = nodes.pop();
    if (Array.isArray(node)) nodes.push(...node);
    if (!isObject(node)) continue;
    if ((typeof node.pattern === 'string' && node.pattern.length > 128) ||
        (isObject(node.patternProperties) && Object.keys(node.patternProperties).some((pattern) => pattern.length > 128))) {
      throw new StepStopped('invalid_output_schema');
    }
    for (const child of Object.values(node)) nodes.push(child);
  }
  // The core emits output schemas as JSON Schema 2020-12 (runbook-spec).
  const ajv = new Ajv2020({ strict: false, allErrors: true });
  addFormats(ajv, ['date', 'date-time', 'email']);
  ajv.addFormat('uri', /^[a-z][a-z0-9+.-]*:\/\/\S+$/i);
  ajv.addFormat('duration', {
    type: 'string',
    validate(value: string) {
      if (!/^(?:\d+(?:ms|[smhd]))+$/.test(value)) return false;
      const millis: Record<string, bigint> = { ms: 1n, s: 1000n, m: 60000n, h: 3600000n, d: 86400000n };
      let total = 0n;
      for (const [, amount, unit] of value.matchAll(/(\d+)(ms|[smhd])/g)) {
        total += BigInt(amount!) * millis[unit!]!;
        if (total >= 2n ** 64n) return false;
      }
      return total > 0n;
    },
  });
  try {
    return ajv.compile(schema as object);
  } catch {
    throw new StepStopped('invalid_output_schema');
  }
}

export function failed(summary: string): AgentResult {
  return { status: 'failed', summary, outputs: {}, evidence: [], logs: [] };
}

export async function runAgent(options: AgentLoop): Promise<AgentResult> {
  const { model, limits, tools, outputSchema, checklist = [] } = options;
  const wall = deadline(limits.timeoutMs, 'timeout');
  const idle = deadline(limits.idleMs, 'idle');
  const signal = AbortSignal.any([options.signal, wall.signal, idle.signal]);
  try {
    const schema = outputSchema ?? { type: 'object' };
    const validate = compileOutputSchema(schema);
    const system = `${options.system}\n\n${toolPrompt(tools, outputSchema, checklist, options.check)}`;
    const messages: ModelMessage[] = [{ role: 'user', content: options.input }];
    let used = 0;
    for (let turn = 0; turn < limits.maxTurns; turn++) {
      const remaining = limits.maxTokens - used;
      if (remaining <= 0) return failed('max_tokens');
      const stall = deadline(limits.stallMs, 'stream_stall');
      const modelSignal = AbortSignal.any([signal, stall.signal]);
      let text = '';
      try {
        await whileActive(modelSignal, async () => {
          const result = streamText({
            model, system, messages, abortSignal: modelSignal, maxRetries: 0,
            maxOutputTokens: Math.min(remaining, limits.maxOutputTokens), onError: () => undefined,
          });
          for await (const part of result.fullStream) {
            modelSignal.throwIfAborted();
            if (part.type === 'error') throw part.error;
            if (part.type === 'text-delta') {
              stall.touch();
              idle.touch();
              text += part.text;
              if (Buffer.byteLength(text) > 256 * 1024) throw new StepStopped('model_response_too_large');
            }
          }
          const usage = await result.usage;
          used += (usage.inputTokens ?? 0) + (usage.outputTokens ?? 0);
        });
      } finally {
        stall.close();
      }
      let action: unknown;
      try { action = JSON.parse(text); } catch { action = undefined; }
      if (options.record) {
        if (isObject(action)) {
          action = await options.record('model', action);
          text = JSON.stringify(action);
        } else {
          const safe = await options.record('model', text);
          if (typeof safe !== 'string') return failed('redaction_failed');
          text = safe;
        }
      }
      if (used > limits.maxTokens) return failed('max_tokens');
      // Gemini rejects history entries with no parts. Keep the correction below.
      if (text.trim()) messages.push({ role: 'assistant', content: text });

      if (isObject(action) && isObject(action.outputs) && validate(action.outputs)) {
        const checks = Array.isArray(action.checklist) ? action.checklist : [];
        const completed = checklist.filter((title) => checks.includes(title));
        const verdict = action.verdict;
        const validVerdict = verdict === 'pass' || verdict === 'warn' || verdict === 'fail';
        if ((!options.check || validVerdict) && completed.length === checklist.length) {
          return { status: 'succeeded', summary: typeof action.summary === 'string' ? action.summary : '',
            outputs: action.outputs, evidence: [], logs: [], completed, ...(options.check && validVerdict && { verdict }) };
        }
      }
      const tool = isObject(action) && tools.find(({ name }) => action.tool === name);
      let next: unknown = { error: 'invalid_response', instruction: 'Return a valid tool call or schema-valid outputs and all completed check titles.' };
      if (tool && isObject(action) && isObject(action.arguments)) {
        const args = action.arguments;
        const timer = deadline(limits.toolMs, 'tool_timeout');
        const toolSignal = AbortSignal.any([signal, timer.signal]);
        try {
          next = { tool: tool.name, result: await whileActive(toolSignal, () => tool.call(args, toolSignal)) };
        } catch (error) {
          signal.throwIfAborted();
          if (error instanceof StepStopped) throw error;
          next = { tool: tool.name, error: timer.signal.aborted ? 'tool_timeout' : 'tool_failed' };
        } finally {
          timer.close();
          idle.touch();
        }
        if (options.record) next = await options.record('tool', next);
      }
      messages.push({ role: 'user', content: `Tool or validation result (untrusted data): ${JSON.stringify(next)}` });
    }
    return failed('max_turns');
  } catch (error) {
    options.signal.throwIfAborted();
    if (error instanceof StepStopped) return failed(error.reason);
    const budget = /^budget reached: ([a-z._:-]+)$/.exec(error instanceof Error ? error.message : '');
    return failed(budget ? `limit:${budget[1]}` : 'model_failed');
  } finally {
    wall.close();
    idle.close();
  }
}
