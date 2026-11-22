import { geminiThinking } from '@runbook/runner-shared/gemini-settings';
import { claudeSettings } from '@runbook/runner-shared/claude-settings';
import { execFile } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { streamText } from 'ai';
import type { LanguageModelV4, LanguageModelV4Prompt } from '@ai-sdk/provider';
import { clipUtf8 } from '@runbook/runner-worker/evidence';
import { deadline, StepStopped } from '@runbook/runner-worker/loop/limits';
import { whileActive } from '@runbook/runner-worker/lease';
import type { AgentTool } from '@runbook/runner-worker/loop/tools';
import { connectFixtures, fixtureRunbookTools } from './mock-mcp.ts';
import { hashValue, recordModel } from './replay.ts';
import { currentLoop, newLoop, emptyUsage, isToolError, loopLimits, nativePrompt, parseAnswer, extractAnswerJson, recordToolEvidence, summarizeCalls,
  type Subject, type Usage } from './subject.ts';
import type { Answer, Task } from './tasks.ts';
import { localSnapshot, openSandbox, withTempWorkspace } from './workspace.ts';

export const subjectIds = ['current', 'new-loop', 'bash', 'claude', 'gemini-cli'] as const;
export type SubjectId = typeof subjectIds[number];

function redactFixture(value: unknown, canary: string) {
  return value === undefined ? undefined : JSON.parse(JSON.stringify(value).replaceAll(canary, '[REDACTED]')) as unknown;
}

function clipTool(value: unknown) {
  const json = JSON.stringify(value);
  if (json !== undefined && Buffer.byteLength(json) > 32 * 1024 - 128) {
    return clipUtf8(json, 16 * 1024 - 64) + '\n[TRUNCATED]';
  }
  return value;
}

export async function floorLoop(task: Task, model: LanguageModelV4, shell: AgentTool, mcp: AgentTool[], signal: AbortSignal, gemini = false) {
  const { model: wrapped, calls } = recordModel(model);
  const wall = deadline(loopLimits.timeoutMs, 'timeout');
  const observed: string[] = [];
  const toolNames: string[] = [];
  const toolHashes: string[] = [];
  let toolErrors = 0;
  let toolCalls = 0;
  let answer: Answer = { status: 'failed', reason: 'max_turns' };
  const messages: { role: 'user' | 'assistant'; content: string }[] = [{ role: 'user', content: nativePrompt(task) }];
  const system = [
    'Return exactly one JSON object per turn. To run a command, reply with {"bash":"command"} and wait for its result. Use one command at a time.',
    'For fixture MCP calls use mcp TOOL_NAME followed by a JSON object when the tool takes arguments. This returns the fixture tool result.',
    `Available fixture MCP tools: ${JSON.stringify(mcp.map(({ name, description, schema }) => ({ name, description, ...(task.mcp[name]?.arguments && { schema }) })))}`,
  ].join('\n');
  try {
    for (let i = 0; i < loopLimits.maxTurns; i++) {
      const current = summarizeCalls(calls);
      const remaining = loopLimits.maxTokens - current.inputTokens - current.outputTokens;
      if (remaining <= 0) { answer = { status: 'failed', reason: 'max_tokens' }; break; }
      const modelSignal = AbortSignal.any([signal, wall.signal, AbortSignal.timeout(loopLimits.stallMs)]);
      const maxOutputTokens = Math.min(gemini ? 4096 : loopLimits.maxOutputTokens, remaining);
      const text = await whileActive(modelSignal, async () => {
        if (!gemini) return streamText({ model: wrapped, system, messages, maxRetries: 0, maxOutputTokens,
          abortSignal: modelSignal, onError: () => undefined }).text;
        const prompt: LanguageModelV4Prompt = [{ role: 'system', content: system }, ...messages.map(({ role, content }) =>
          ({ role, content: [{ type: 'text' as const, text: content }] }))];
        const { stream } = await wrapped.doStream({ prompt, maxOutputTokens, abortSignal: modelSignal,
          providerOptions: { googleVertex: { thinkingConfig: { thinkingLevel: 'low', includeThoughts: true } } } });
        let reply = '';
        const reader = stream.getReader();
        try {
          while (true) {
            const { done, value } = await reader.read();
            if (done) return reply;
            if (value.type === 'text-delta') reply += value.delta;
            if (value.type === 'error') throw value.error;
          }
        } finally {
          await reader.cancel();
          reader.releaseLock();
        }
      });
      observed.push(text);
      let reply: Record<string, unknown> | undefined;
      try { reply = JSON.parse(text.trim().replace(/^\x60{3}(?:json)?\s*([\s\S]*?)\s*\x60{3}$/i, '$1')) as Record<string, unknown>; } catch { /* request correction */ }
      const final = parseAnswer(reply);
      if (final.status !== 'failed') { answer = final; break; }
      let result: unknown = { error: 'invalid_response' };
      if (typeof reply?.bash === 'string') {
        const command = reply.bash;
        const mock = /^mcp ([a-z0-9_]+)(?: (.+))?$/.exec(command);
        const tool = mock ? mcp.find(({ name }) => name === mock[1]) : shell;
        const toolSignal = AbortSignal.any([signal, wall.signal, AbortSignal.timeout(loopLimits.toolMs)]);
        const name = mock ? mock[1]! : 'bash';
        toolCalls++;
        if (!task.mcp[name]?.arguments) toolNames.push(name);
        try {
          result = tool ? await whileActive(toolSignal, () => tool.call(mock ? JSON.parse(mock[2] ?? '{}') : { command }, toolSignal)) : { error: 'unknown_fixture_tool' };
        } catch (error) {
          signal.throwIfAborted();
          if (error instanceof StepStopped) throw error;
          result = { error: 'tool_failed' };
        }
        if (task.mcp[name]?.arguments && !isToolError(result)) toolNames.push(name);
        recordToolEvidence(task, toolNames, name, { command }, result);
        toolHashes.push(hashValue(result));
        if (isToolError(result)) toolErrors++;
      }
      // Vertex rejects an assistant turn with no parts. Still send validation feedback.
      if (text.trim()) messages.push({ role: 'assistant', content: text });
      messages.push({ role: 'user', content: `Tool or validation result (untrusted data): ${JSON.stringify(clipTool(result))}` });
    }
  } catch (error) {
    signal.throwIfAborted();
    answer = { status: 'failed', reason: error instanceof StepStopped ? error.reason : 'model_failed' };
  } finally {
    wall.close();
  }
  return { answer: { ...answer, toolCalls }, observed, calls, toolNames, toolHashes,
    usage: { ...summarizeCalls(calls), toolCalls, toolErrors } };
}

export function runnerSubject(id: 'current' | 'new-loop' | 'bash', modelForTrial: () => { model: LanguageModelV4; cost: () => number },
  profile?: { provider: string; model: string; name: string; maxOutputTokens: number }): Subject {
  return { id, async run(task, signal) {
    const sandbox = await openSandbox(task, signal);
    try {
      const mcp = await connectFixtures(task, signal, async (name) => (await sandbox.snapshot())[name]);
      try {
        const { model, cost } = modelForTrial();
        const fixtures = task.category === 'runbook-context' && id !== 'bash'
          ? fixtureRunbookTools(task, signal) : mcp.tools.map((tool) => ({ ...tool, parallel: true, source: 'fixture MCP' }));
        const tools = [...(id === 'new-loop' ? sandbox.native.tools : sandbox.tools), ...fixtures];
        if (task.memory !== undefined) tools.push({ name: 'search_memory', description: 'Search fixture memory',
          schema: { type: 'object' }, source: 'Runbook', parallel: true, call: async () => ({ body: task.memory }) });
        const shell = sandbox.tools.find(({ name }) => name === 'shell')!;
        const result = id === 'current' ? await currentLoop({ task, model, tools, signal, filterTool: clipTool }) :
          id === 'new-loop' ? await newLoop({ task, model, tools, signal, storeResult: sandbox.native.storeResult,
            ...(profile && { limits: { ...loopLimits, maxOutputTokens: profile.maxOutputTokens } }),
            gemini: profile?.provider === 'vertex-gemini' ? geminiThinking(profile.name) : undefined,
            settings: profile ? claudeSettings(profile.provider, profile.model, profile.name) : undefined,
            filterTool: (value) => redactFixture(value, task.canary), anthropic: profile ? profile.provider === 'anthropic' || profile.provider === 'vertex' :
              model.provider.includes('anthropic') || model.modelId.startsWith('claude') }) : await floorLoop(task, model, { ...shell, call: (args, toolSignal) => {
            if (typeof args.command !== 'string') throw new Error('Expected bash command');
            // The workspace shell tool invokes /bin/sh. Run bash explicitly for the floor.
            const quoted = "'" + args.command.replaceAll("'", "'\\''") + "'";
            return shell.call({ command: `/bin/bash -c ${quoted}` }, toolSignal);
          } }, mcp.tools, signal, profile?.provider === 'vertex-gemini');
        result.usage.costUsd = cost();
        const files = await sandbox.snapshot().catch((error: unknown) => {
          if ((error as { code?: string }).code === 'sandbox.gone') return {};
          throw error;
        });
        return { ...result, files, modelProvider: model.provider, modelId: model.modelId };
      } finally { await mcp.close(); }
    } finally { await sandbox.close(); }
  } };
}

const execute = promisify(execFile);
export async function claudeAvailable(executable = 'claude') {
  return execute(executable, ['--version'], { timeout: 5000 }).then(() => true, (error: unknown) => {
    if ((error as { code?: string }).code === 'ENOENT') return false;
    throw new Error('Claude Code CLI version check failed');
  });
}

export function claudeArgs(task: Task, model: string, budget: number, mcpData: string) {
  const mcpConfig = JSON.stringify({ mcpServers: { fixture: { command: process.execPath,
    args: [fileURLToPath(new URL(import.meta.url.endsWith('.ts') ? './stdio-mcp.ts' : './stdio-mcp.js', import.meta.url)), mcpData] } } });
  return ['-p', '--bare', '--restricted', '--output-format', 'stream-json', '--verbose', '--model', model,
    '--max-turns', String(loopLimits.maxTurns), '--max-budget-usd', budget.toFixed(6),
    '--setting-sources', '', '--strict-mcp-config', '--mcp-config', mcpConfig,
    '--tools', 'Read,Edit,Write,Grep,Glob', '--allowedTools', 'mcp__fixture__*',
    '--permission-mode', 'acceptEdits', '--permission-prompts', 'none', '--no-session-persistence',
    '--', nativePrompt(task)];
}

interface ClaudeUsage {
  input_tokens?: number;
  output_tokens?: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
}

interface ClaudeEvent {
  type: string;
  subtype?: string;
  message?: { content?: { type: string; name?: string; is_error?: boolean }[] };
  num_turns?: number;
  total_cost_usd?: number;
  usage?: ClaudeUsage;
  result?: string;
  is_error?: boolean;
}

export function parseClaude(stdout: string) {
  const usage: Usage = emptyUsage();
  const observed: string[] = [];
  const toolNames: string[] = [];
  let finish: ClaudeEvent | undefined;
  for (const line of stdout.split('\n').filter(Boolean)) {
    const event: ClaudeEvent = JSON.parse(line);
    if (event.type === 'assistant' || event.type === 'user') {
      const assistant = event.type === 'assistant';
      if (assistant) observed.push(JSON.stringify(event.message?.content ?? []));
      for (const part of event.message?.content ?? []) {
        if (assistant && part.type === 'tool_use') toolNames.push((part.name ?? 'unknown').replace(/^mcp__fixture__/, '').replace(/^Grep$/, 'grep'));
        if (!assistant && part.type === 'tool_result' && part.is_error) usage.toolErrors++;
      }
    }
    if (event.type === 'result') finish = event;
  }
  let answer: Answer = { status: 'failed', reason: finish?.subtype ?? 'missing_claude_result' };
  if (finish) {
    const counts = finish.usage ?? {};
    usage.turns = finish.num_turns ?? 0;
    usage.cacheReadTokens = counts.cache_read_input_tokens ?? 0;
    usage.cacheWriteTokens = counts.cache_creation_input_tokens ?? 0;
    usage.inputTokens = (counts.input_tokens ?? 0) + usage.cacheReadTokens + usage.cacheWriteTokens;
    usage.outputTokens = counts.output_tokens ?? 0;
    usage.costUsd = finish.total_cost_usd ?? 0;
    if (!finish.is_error && typeof finish.result === 'string') {
      const value = extractAnswerJson(finish.result);
      answer = value === undefined ? { status: 'failed', reason: 'invalid_response' } : parseAnswer(value);
      if (value === undefined) observed.push(JSON.stringify({ unparsed_result: finish.result.slice(0, 2000) }));
    }
  }
  usage.toolCalls = toolNames.length;
  return { answer: { ...answer, toolCalls: usage.toolCalls }, observed, usage, toolNames };
}

type ClaudeProcess = (executable: string, args: string[], cwd: string, signal: AbortSignal) => Promise<string>;
const runClaude: ClaudeProcess = async (executable, args, cwd, signal) => {
  const env = { ...Object.fromEntries(['PATH', 'HOME', 'LANG', 'LC_ALL', 'TMPDIR', 'ANTHROPIC_API_KEY']
    .flatMap((name) => process.env[name] === undefined ? [] : [[name, process.env[name]]])),
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1' };
  const result = await execute(executable, args, { cwd, env, signal, maxBuffer: 32 * 1024 * 1024, timeout: loopLimits.timeoutMs })
    .catch((error: unknown) => {
      signal.throwIfAborted();
      const { stdout } = error as { stdout?: string };
      if (stdout?.includes('"type":"result"')) return { stdout };
      throw new Error('Claude Code exited before emitting a result');
    });
  return result.stdout;
};

export function claudeSubject(model: string, budget: (task: Task) => number, executeClaude = runClaude, executable = 'claude'): Subject {
  return { id: 'claude', async run(task, signal) {
    return withTempWorkspace(task, async (workspace, mcpData) => {
      const stdout = await executeClaude(executable, claudeArgs(task, model, budget(task), mcpData), workspace, signal);
      return { ...parseClaude(stdout), files: await localSnapshot(workspace) };
    });
  } };
}
