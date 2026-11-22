import { readFile } from 'node:fs/promises';
import { expect, it } from 'vitest';
import { claudeSubject, floorLoop } from '../src/baselines.ts';
import { connectFixtures } from '../src/mock-mcp.ts';
import { grade, selectTasks } from '../src/tasks.ts';
import { finish, readTool, reply, scripted, task } from './scripted.ts';

const signal = new AbortController().signal;

it('runs a bash command and decodes a blocked response via the fixture MCP server', async () => {
  const blocked = selectTasks('access/billing')[0]!;
  const mcp = await connectFixtures(blocked, signal);
  const commands: unknown[] = [];
  const shell = { ...readTool(), async call(args: Record<string, unknown>) {
    commands.push(args.command);
    return { exit_code: 0, stdout: blocked.files['scope.txt'] };
  } };
  try {
    const result = await floorLoop(blocked, scripted(reply({ bash: 'cat scope.txt' }), reply({ bash: 'mcp connector' }),
      reply({ status: 'blocked', kind: 'missing_access', summary: 'The connector denied billing access.' })), shell, mcp.tools, signal);
    expect(commands).toEqual(['cat scope.txt']);
    expect(result.usage).toMatchObject({ turns: 3, toolCalls: 2, toolErrors: 1 });
    expect(grade(blocked, result.answer, blocked.files, result.observed, result.toolNames).success).toBe(true);
  } finally { await mcp.close(); }
});

it('passes Claude Code a fresh workspace, fixture MCP, model, and caps, and parses stream-json', async () => {
  let cwd: string | undefined;
  const subject = claudeSubject('claude-fixture', () => 0.15, async (executable, args, workspace) => {
    cwd = workspace;
    expect(executable).toBe('claude');
    expect(args).toEqual(expect.arrayContaining(['-p', 'stream-json', 'claude-fixture', '--max-turns', '--max-budget-usd', '0.150000']));
    expect(await readFile(`${workspace}/service.log`, 'utf8')).toBe(task.files['service.log']);
    const config = JSON.parse(args[args.indexOf('--mcp-config') + 1]!);
    expect(JSON.parse(await readFile(config.mcpServers.fixture.args[1], 'utf8'))).toEqual(task.mcp);
    return [
      { type: 'assistant', message: { content: [{ type: 'tool_use', name: 'Read' }] } },
      { type: 'user', message: { content: [{ type: 'tool_result', is_error: false }] } },
      { type: 'result', subtype: 'success', num_turns: 2, total_cost_usd: 0.012,
        usage: { input_tokens: 10, cache_read_input_tokens: 7, cache_creation_input_tokens: 2, output_tokens: 5 },
        result: JSON.stringify(finish) },
    ].map((event) => JSON.stringify(event)).join('\n');
  });
  const result = await subject.run(task, signal);
  expect(result.usage).toMatchObject({ costUsd: 0.012, toolCalls: 1, turns: 2, inputTokens: 19, cacheReadTokens: 7 });
  expect(grade(task, result.answer, result.files).success).toBe(true);
  await expect(readFile(`${cwd}/service.log`)).rejects.toMatchObject({ code: 'ENOENT' });
});

it('serves the same mock connector over the stdio transport configured for Claude Code', async () => {
  const { Client } = await import('@modelcontextprotocol/sdk/client/index.js');
  const { StdioClientTransport } = await import('@modelcontextprotocol/sdk/client/stdio.js');
  const blocked = selectTasks('access/billing')[0]!;
  const subject = claudeSubject('fixture-model', () => 0.1, async (_executable, args) => {
    const config = JSON.parse(args[args.indexOf('--mcp-config') + 1]!);
    const client = new Client({ name: 'eval-test', version: '1.0.0' });
    const transport = new StdioClientTransport(config.mcpServers.fixture);
    try {
      await client.connect(transport, { signal });
      expect((await client.listTools()).tools.map(({ name }) => name)).toEqual(['connector']);
      expect(await client.callTool({ name: 'connector' })).toMatchObject({
        isError: true, content: [{ type: 'text', text: JSON.stringify(blocked.mcp['connector']!.data) }],
      });
    } finally { await client.close(); }
    return '';
  });
  await subject.run(blocked, signal);
});

it('recovers from an empty Gemini reply without replaying an invalid assistant turn', async () => {
  const { budgetedGateway, profileFor } = await import('../src/spend.ts');
  const profile = profileFor('default', { gemini: true, project: 'fixture', location: 'global' });
  let calls = 0;
  const { model } = budgetedGateway('default', profile, 'fixture', 1, async (_profile, request) => {
    calls++;
    if (request.messages?.some(({ role, content }) => role === 'assistant' && !content.trim())) {
      throw new Error('Vertex rejected a model turn with no parts');
    }
    if (calls === 2) expect(request.messages?.at(-1)?.content).toContain('invalid_response');
    const text = calls === 1 ? '' : calls === 2 ? '{"bash":"cat service.log"}' : JSON.stringify(finish);
    return { text, inputTokens: 100, outputTokens: 70 };
  });
  const result = await floorLoop(task, model, readTool(), [], signal);
  expect(result.answer).toMatchObject({ status: 'submitted', toolCalls: 1 });
  expect(calls).toBe(3);
});

it('withholds guarded edit evidence when the workspace was changed before the precondition check', async () => {
  const guarded = selectTasks('guarded-edit/migration')[0]!;
  let files = { ...guarded.files };
  const mcp = await connectFixtures(guarded, signal, async (name) => files[name]);
  const generation = mcp.tools.find(({ name }) => name === 'generation')!;
  try {
    expect(await generation.call({}, signal)).not.toMatchObject({ isError: true });
    files = { ...files, ...guarded.expected.edits };
    expect(await generation.call({}, signal)).toMatchObject({ isError: true });
  } finally { await mcp.close(); }
});

it('recovers from Hydrogen malformed-function finishes and executes complete JSON text', async () => {
  const { partStream, streamingModel } = await import('../src/replay.ts');
  // Reduced from the logs/disk Vertex trace. The first finish had no text
  // deltas, and a later finish with the same reason followed complete JSON.
  const malformed = {
    type: 'finish' as const, finishReason: { unified: 'error' as const, raw: 'MALFORMED_FUNCTION_CALL' },
    usage: {
      inputTokens: { total: 360, noCache: 360, cacheRead: 0, cacheWrite: undefined },
      outputTokens: { total: 42, text: 30, reasoning: 12 },
    },
  };
  const command = 'ls -la service.log && head -n 50 service.log';
  let calls = 0;
  const model = streamingModel('gemini-4-hydrogen', async ({ prompt, maxOutputTokens }) => {
    expect(maxOutputTokens).toBe(4096);
    calls++;
    if (calls === 1) return { stream: partStream([malformed]) };
    if (calls === 2) {
      expect(prompt.map(({ role }) => role)).toEqual(['system', 'user', 'user']);
      return { stream: partStream([
        { type: 'text-delta', id: 'text', delta: '{"' },
        { type: 'text-delta', id: 'text', delta: `bash":${JSON.stringify(command)}}` }, malformed,
      ]) };
    }
    return { stream: partStream([
      { type: 'text-delta', id: 'text', delta: JSON.stringify(finish) },
      { ...malformed, finishReason: { unified: 'stop', raw: 'STOP' } },
    ]) };
  });
  const executed: unknown[] = [];
  const shell = { ...readTool(), async call(args: Record<string, unknown>) {
    executed.push(args.command);
    return { exit_code: 0, stdout: task.files['service.log'] };
  } };
  const result = await floorLoop(task, model, shell, [], signal, true);
  expect(executed).toEqual([command]);
  expect(result.answer).toMatchObject({ status: 'submitted', toolCalls: 1 });
  expect(grade(task, result.answer, task.files, result.observed, result.toolNames).success).toBe(true);
});
