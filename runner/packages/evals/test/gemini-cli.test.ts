import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { expect, it } from 'vitest';
import { geminiSubject } from '../src/gemini-cli.ts';
import { finish, task } from './scripted.ts';

const profile = { provider: 'vertex-gemini', model: 'gemini-4-hydrogen', credentialId: 'eval', endpoint: '',
  maxOutputTokens: 4096, inputUsdPerMillion: 1.5, outputUsdPerMillion: 7.5, cacheReadUsdPerMillion: 0.075 } as const;

it('uses a pinned isolated Vertex CLI, the fixture MCP, and billed reasoning and cache counts', async () => {
  const subject = geminiSubject(profile, { project: 'angler-dev', location: 'global' }, () => 1,
    async (args, env, cwd) => {
      expect(args.slice(0, 2)).toEqual(['-y', '@google/gemini-cli@0.70.0']);
      expect(args).toContain('stream-json');
      expect(env.GOOGLE_CLOUD_LOCATION).toBe('global');
      expect(env.GOOGLE_CLOUD_QUOTA_PROJECT).toBe('angler-dev');
      expect(env.GEMINI_CLI_CUSTOM_HEADERS).toBe('x-goog-user-project: angler-dev');
      const settings = JSON.parse(await readFile(join(env.GEMINI_CLI_HOME!, '.gemini', 'settings.json'), 'utf8'));
      expect(settings.model).toMatchObject({ name: profile.model, maxSessionTurns: 65 });
      expect(settings.tools.core).not.toContain('run_shell_command');
      expect(JSON.parse(await readFile(settings.mcpServers.fixture.args.at(-1), 'utf8'))).toEqual(task.mcp);
      expect(await readFile(join(cwd, 'service.log'), 'utf8')).toBe(task.files['service.log']);
      return { stdout: [
        { type: 'tool_use', tool_name: 'mcp_fixture_lookup' },
        { type: 'tool_result', status: 'error' },
        { type: 'message', role: 'assistant', delta: true, content: JSON.stringify(finish) },
        { type: 'result', status: 'success', stats: { input_tokens: 100, cached: 50, output_tokens: 20, total_tokens: 140 } },
      ].map((line) => JSON.stringify(line)).join('\n') };
    });
  const result = await subject.run(task, new AbortController().signal);
  expect(result.answer).toMatchObject({ status: 'submitted', outputs: finish.outputs });
  expect(result.toolNames).toEqual(['lookup']);
  expect(result.usage).toMatchObject({ turns: 2, toolCalls: 1, toolErrors: 1, inputTokens: 100, outputTokens: 40, cacheReadTokens: 50 });
  expect(result.usage.costUsd).toBeCloseTo(0.00037875, 10);
});

it('reserves Gemini CLI requests before fetch and settles Vertex usage before the next request', async () => {
  const { guardGeminiFetch } = await import('../src/gemini-budget.ts');
  const endpoint = 'https://aiplatform.googleapis.com/v1/projects/test/locations/global/publishers/google/models';
  const url = endpoint + '/gemini-4-hydrogen:streamGenerateContent?alt=sse';
  const calls: string[] = [];
  const ledger: [number, boolean][] = [];
  const fetcher = guardGeminiFetch(async (url) => {
    calls.push(String(url));
    return new Response('data: {"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":20,"thoughtsTokenCount":30,"totalTokenCount":150,"cachedContentTokenCount":40}}\n\n',
      { headers: { 'content-type': 'text/event-stream' } });
  }, { endpoint, model: 'gemini-4-hydrogen', cap: 0.003, input: 1.5, output: 7.5, cacheRead: 0.075, maxOutput: 100 },
  (spent, denied) => ledger.push([spent, denied]));
  const body = JSON.stringify({ generationConfig: { maxOutputTokens: 100 } });
  expect((await fetcher(url, { method: 'POST', body })).status).toBe(200);
  expect(ledger.at(-1)).toEqual([0.000468, false]);
  expect((await fetcher(url, { method: 'POST', body })).status).toBe(200);
  expect((await fetcher(url, { method: 'POST', body })).status).toBe(402);
  expect(calls).toHaveLength(2);
  expect(ledger.at(-1)).toEqual([0.000936, true]);
  expect((await fetcher(url.replace('gemini-4-hydrogen', 'other'), { method: 'POST', body })).status).toBe(403);
});
