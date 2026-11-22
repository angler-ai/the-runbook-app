import { readFileSync } from 'node:fs';
import { geminiThinking } from '@runbook/runner-shared/gemini-settings';
import { claudeSettings } from '@runbook/runner-shared/claude-settings';
import type { LanguageModelV4StreamPart } from '@ai-sdk/provider';
import { replayModel, type ModelCall } from '../src/replay.ts';
import { expect, it } from 'vitest';
import type { AgentTool } from '@runbook/runner-worker/loop/tools';
import { fixtureRunbookTools } from '../src/mock-mcp.ts';
import { newLoop } from '../src/subject.ts';
import { grade, tasks } from '../src/tasks.ts';
import { nativeReply, scripted } from './scripted.ts';

async function runStructure(task: (typeof tasks)[number], recording?: ModelCall[], gemini = false) {
  const signal = new AbortController().signal;
  const archives = new Map<string, string>();
  const context = !gemini && task.category === 'long-context';
  const settings = context ? claudeSettings('vertex', 'claude-sonnet-5-5', 'default') : undefined;
  const artifact = '/runbook-results/call-2.txt';
  const files = { ...task.files };
  const commands = task.expected.verification;
  const tools: AgentTool[] = [
    ...['bash', 'bash_background', 'bash_output', 'grep'].map((name): AgentTool => ({
      name, description: name, schema: { type: 'object' },
      async call(args: Record<string, unknown>) {
        if (name === 'bash') {
          expect(args.command).toBe(commands?.command);
          return { exit_code: 0, stdout: commands?.stdout };
        }
        if (name === 'bash_background') return { job_id: '1' };
        if (name === 'bash_output') return { exit_code: 0, running: false };
        return 'matching records';
      },
    })).filter(({ name }) => task.difficulty !== 'standard' &&
      (name === 'bash' ? commands : task.category === 'background' ? name !== 'grep' : task.category === 'filtered-log' && name === 'grep')),
    { name: 'view', description: 'Read a file', schema: { type: 'object' }, parallel: true,
      async call(args: Record<string, unknown>) {
        const path = String(args.path);
        if (path === artifact) {
          const text = archives.get(path);
          expect(text).toContain('normalized event fields');
          return text;
        }
        return files[path];
      } },
    { name: 'str_replace', description: 'Update a fixture', schema: { type: 'object' },
      async call(args: Record<string, unknown>) {
        const path = String(args.path);
        files[path] = files[path]!.replace(String(args.old_str), String(args.new_str));
        return { ok: true };
      } },
    ...(task.category === 'runbook-context' ? fixtureRunbookTools(task, signal) : Object.entries(task.mcp).map(([name, item]) => ({
      name, description: item.description, schema: { type: 'object' }, parallel: true, source: 'Fixture MCP',
      async call(args: Record<string, unknown>) {
        expect(args).toEqual(item.arguments ?? {});
        return item.data;
      },
    }))),
  ];
  const reads = Object.keys(task.files).slice(0, task.difficulty === 'standard' ? 2 : undefined).map((path, i) => nativeReply('view', { path }, `view-${i}`));
  const mcp = (task.expected.requiredTools ?? []).filter((name) => Object.hasOwn(task.mcp, name))
    .map((name, i) => nativeReply(name, task.mcp[name]?.arguments ?? {}, `mcp-${i}`));
  const extra = commands ? [nativeReply('bash', { command: commands.command }, 'verify')]
    : task.category === 'background' ? [
      nativeReply('bash_background', { command: 'bash job.sh' }, 'start'),
      nativeReply('bash_output', { job_id: '1' }, 'poll'),
    ] : task.category === 'filtered-log' ? [nativeReply('grep', { pattern: 'operation' }, 'filter')] : [];
  if (context) {
    const turn = mcp[14]!;
    const finish = turn.at(-1) as Extract<LanguageModelV4StreamPart, { type: 'finish' }>;
    turn[turn.length - 1] = {
      ...finish,
      usage: { ...finish.usage, inputTokens: { total: 101_000, noCache: 1000, cacheRead: 100_000, cacheWrite: 0 } },
      providerMetadata: { anthropic: { contextManagement: { appliedEdits: [
        { type: 'clear_tool_uses_20250919', clearedToolUses: 8, clearedInputTokens: 24_000 },
      ] } } },
    };
  }
  const edits = Object.entries(task.expected.edits ?? {}).map(([path, after], i) =>
    nativeReply('str_replace', { path, old_str: task.files[path], new_str: after }, `edit-${i}`));
  const finish = task.expected.kind ? nativeReply('report_blocked', {
    kind: task.expected.kind, reason: task.expected.blockedSummary ? JSON.stringify(task.expected.blockedSummary) : 'Required evidence is unavailable.', details: 'Inspected the supplied fixture data.',
  }, 'finish') : nativeReply('submit_result', {
    outputs: context || gemini ? JSON.stringify(task.expected.outputs) : task.expected.outputs, summary: 'Verified the fixture result.', unverified: [],
    evidence: [{ claim: 'Read the supplied evidence', tool_call_ids: [reads.length ? 'view-0' : 'mcp-0'] }],
    checklist: task.step.checklist, verdict: task.expected.verdict,
  }, 'finish');
  const script = [...reads, ...mcp, ...edits, ...extra];
  if (gemini) {
    for (const turn of [...script, finish]) {
      const end = turn.at(-1) as Extract<LanguageModelV4StreamPart, { type: 'finish' }>;
      end.finishReason.raw = 'STOP';
    }
  }
  const observedRequests: string[] = [];
  const reRead = context ? [nativeReply('view', { path: artifact }, 'archive-read')] : [];
  const replay = recording && replayModel('fixture', recording);
  const result = await newLoop({
    task, model: replay ? replay.model : scripted(...script, ...reRead, finish),
    tools, signal, settings, gemini: gemini ? geminiThinking('default') : undefined,
    ...(settings && { async storeResult(id: string, body: string) {
      const path = '/runbook-results/' + id.replace(' ', '-') + '.txt';
      archives.set(path, body);
      return path;
    } }),
    filterTool(value) { return JSON.parse(JSON.stringify(value).replaceAll(task.canary, '[REDACTED]')); },
    beforeCall(call) {
      observedRequests.push(JSON.stringify(call.prompt));
      if (gemini) expect(call.providerOptions).toEqual({ googleVertex: { thinkingConfig: { thinkingLevel: 'medium' } } });
      if (context) expect(call.providerOptions?.anthropic?.contextManagement).toEqual(settings?.modelOptions.contextManagement);
    },
  });
  expect(grade(task, result.answer, files, result.observed, result.toolNames)).toMatchObject({ success: true });
  if (task.previousAttempt) expect(observedRequests[0]).toContain(task.previousAttempt);
  if (context) {
    expect(result.observed.find((entry) => entry.includes('clearedInputTokens'))).toContain('24000');
    expect(archives.get(artifact)).toContain('normalized event fields');
    expect(result.toolNames.at(-1)).toBe('view');
  }
  replay?.assertComplete();
  return result;
}

it.each(tasks)('runs the $id fixture through the native loop and structural grader', async (task) => {
  await runStructure(task);
});

it('replays a long-context run that reopens an archived result after server-side clearing', async () => {
  const task = tasks.find(({ id }) => id.startsWith('long-context/archive'))!;
  const url = new URL('../fixtures/v2-context-editing.json', import.meta.url);
  const calls = JSON.parse(readFileSync(url, 'utf8')) as ModelCall[];
  await runStructure(task, calls);
});

it.each(tasks)('runs the $id fixture with fake Gemini STOP/tool turns', async (task) => {
  await runStructure(task, undefined, true);
});
