import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { promisify } from 'node:util';
import type { ModelProfile } from '@runbook/runner-gateway/model/endpoint';
import { usageCost } from '@runbook/runner-gateway/model/stream';
import { emptyUsage, loopLimits, nativePrompt, parseAnswer, extractAnswerJson, type Subject } from './subject.ts';
import type { Answer, Task } from './tasks.ts';
import { localSnapshot, withTempWorkspace } from './workspace.ts';
import { stdioMcpArgs } from './stdio-args.ts';

const execute = promisify(execFile);
const pinned = ['-y', '@google/gemini-cli@0.70.0'];
export type VertexTarget = { project: string; location: string };

export async function geminiAvailable(signal?: AbortSignal) {
  return execute('npx', [...pinned, '--version'], { timeout: 120_000, signal }).then(
    ({ stdout }) => stdout.trim().split('\n').at(-1) === '0.70.0',
    () => { signal?.throwIfAborted(); return false; },
  );
}

interface GeminiEvent {
  type: string;
  role?: string;
  content?: string;
  delta?: boolean;
  tool_name?: string;
  output?: string;
  status?: string;
  stats?: { input_tokens?: number; output_tokens?: number; total_tokens?: number; cached?: number;
    models?: Record<string, { total_tokens?: number }> };
}

export function parseGemini(stdout: string, profile: ModelProfile, budget: number, exitCode: number | undefined) {
  const usage = emptyUsage();
  const observed: string[] = [];
  const toolNames: string[] = [];
  let response = '';
  let inToolBatch = false;
  let finish: GeminiEvent | undefined;
  for (const line of stdout.split('\n')) {
    if (!line.trimStart().startsWith('{')) continue;
    const event: GeminiEvent = JSON.parse(line);
    if (event.type === 'message' && event.role === 'assistant') {
      if (!event.delta || inToolBatch || !response) usage.turns++;
      if (!event.delta || inToolBatch) response = '';
      response += event.content ?? '';
      inToolBatch = false;
      observed.push(event.content ?? '');
    } else if (event.type === 'tool_use') {
      if (!inToolBatch && !response) usage.turns++;
      inToolBatch = true;
      response = '';
      toolNames.push((event.tool_name ?? 'unknown').replace(/^(?:mcp__fixture__|mcp_fixture_)/, '').replace(/^grep_search$/, 'grep'));
      observed.push(line);
    } else if (event.type === 'tool_result') {
      if (event.status === 'error') usage.toolErrors++;
    } else if (event.type === 'result') finish = event;
  }
  const counts = finish?.stats;
  const knownModels = Object.entries(counts?.models ?? {}).filter(([, stats]) => (stats.total_tokens ?? 0) > 0);
  const wrongModel = knownModels.some(([model]) => model !== profile.model && model !== `models/${profile.model}`);
  usage.inputTokens = counts?.input_tokens ?? 0;
  // The CLI's output_tokens excludes thought tokens. Its total includes them.
  usage.outputTokens = Math.max(counts?.output_tokens ?? 0, (counts?.total_tokens ?? 0) - usage.inputTokens);
  usage.cacheReadTokens = counts?.cached ?? 0;
  usage.toolCalls = toolNames.length;
  usage.costUsd = counts ? usageCost(profile, {
    inputTokens: { total: usage.inputTokens, noCache: undefined, cacheRead: usage.cacheReadTokens, cacheWrite: 0 },
    outputTokens: { total: usage.outputTokens, text: undefined, reasoning: undefined },
  }) : budget;
  let answer: Answer = { status: 'failed', reason: wrongModel ? 'unexpected_gemini_model' :
    exitCode === 53 ? 'max_turns' : 'missing_gemini_result' };
  if (!wrongModel && finish?.status === 'success' && counts) {
    const value = extractAnswerJson(response);
    answer = value === undefined ? { status: 'failed', reason: 'invalid_response' } : parseAnswer(value);
    if (value === undefined) observed.push(JSON.stringify({ unparsed_result: response.slice(0, 2000) }));
  }
  return { answer: { ...answer, toolCalls: usage.toolCalls }, observed, usage, toolNames };
}

type GeminiProcess = (args: string[], env: NodeJS.ProcessEnv, cwd: string, signal: AbortSignal) =>
  Promise<{ stdout: string; exitCode?: number }>;

const runGemini: GeminiProcess = async (args, env, cwd, signal) => {
  signal.throwIfAborted();
  const deadline = AbortSignal.any([signal, AbortSignal.timeout(loopLimits.timeoutMs)]);
  const child = spawn('npx', args, { cwd, env, stdio: ['ignore', 'pipe', 'ignore'], detached: true });
  const kill = () => {
    if (child.pid === undefined) return;
    try { process.kill(-child.pid, 'SIGKILL'); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error; }
  };
  deadline.addEventListener('abort', kill, { once: true });
  const chunks: Buffer[] = [];
  let bytes = 0;
  child.stdout.on('data', (chunk: Buffer) => {
    bytes += chunk.length;
    if (bytes > 32 * 1024 * 1024) kill();
    else chunks.push(chunk);
  });
  try {
    const [exitCode] = await once(child, 'close') as [number | null];
    signal.throwIfAborted();
    return { stdout: Buffer.concat(chunks).toString('utf8'), exitCode: exitCode ?? undefined };
  } finally {
    deadline.removeEventListener('abort', kill);
  }
};

export function geminiSubject(profile: ModelProfile, target: VertexTarget, budget: (task: Task) => number, executeGemini = runGemini): Subject {
  return { id: 'gemini-cli', async run(task, signal) {
    return withTempWorkspace(task, async (workspace, mcpData) => {
      const home = dirname(workspace);
      const config = join(home, '.gemini');
      const settings = join(config, 'settings.json');
      await mkdir(config);
      await writeFile(settings, JSON.stringify({
        security: { auth: { selectedType: 'vertex-ai' } },
        privacy: { usageStatisticsEnabled: false },
        model: { name: profile.model, maxSessionTurns: loopLimits.maxTurns },
        modelConfigs: { customOverrides: [{ match: { model: profile.model },
          modelConfig: { generateContentConfig: { maxOutputTokens: profile.maxOutputTokens, thinkingConfig: { thinkingLevel: 'LOW' } } },
        }] },
        tools: { core: ['read_file', 'list_directory', 'grep_search', 'glob', 'write_file', 'replace',
          ...Object.keys(task.mcp).map((name) => `mcp_fixture_${name}`)] },
        admin: { extensions: { enabled: false }, skills: { enabled: false } },
        mcpServers: { fixture: { trust: true, command: process.execPath, args: [
          ...stdioMcpArgs(), mcpData,
        ] } },
      }), { mode: 0o600 });
      const ledger = join(home, 'spend.json');
      const cap = budget(task);
      const preload = new URL(import.meta.url.endsWith('.ts') ? './gemini-budget.ts' : './gemini-budget.js', import.meta.url);
      const env = { ...Object.fromEntries(['PATH', 'HOME', 'LANG', 'LC_ALL', 'TMPDIR', 'GOOGLE_APPLICATION_CREDENTIALS', 'CLOUDSDK_CONFIG']
        .flatMap((name) => process.env[name] === undefined ? [] : [[name, process.env[name]]])),
        NODE_OPTIONS: `--import=${preload.href}`,
        RUNBOOK_EVAL_GEMINI_LEDGER: ledger,
        RUNBOOK_EVAL_GEMINI_LIMITS: JSON.stringify({ endpoint: profile.endpoint, model: profile.model, cap,
          input: profile.inputUsdPerMillion, output: profile.outputUsdPerMillion,
          cacheRead: profile.cacheReadUsdPerMillion ?? profile.inputUsdPerMillion, maxOutput: profile.maxOutputTokens }),
        GEMINI_CLI_HOME: home, GEMINI_CLI_SYSTEM_SETTINGS_PATH: join(home, 'system-settings.json'),
        GEMINI_CLI_SYSTEM_DEFAULTS_PATH: join(home, 'system-defaults.json'),
        GOOGLE_GENAI_USE_VERTEXAI: 'true', GOOGLE_CLOUD_PROJECT: target.project, GOOGLE_CLOUD_LOCATION: target.location,
        GOOGLE_CLOUD_QUOTA_PROJECT: target.project, GEMINI_CLI_CUSTOM_HEADERS: `x-goog-user-project: ${target.project}`,
        GOOGLE_GENAI_API_VERSION: 'v1',
      };
      const { stdout, exitCode } = await executeGemini([...pinned, '--model', profile.model, '--approval-mode', 'auto_edit',
        '--skip-trust', '--output-format', 'stream-json', '--prompt', nativePrompt(task)], env, workspace, signal);
      const result = parseGemini(stdout, profile, cap, exitCode);
      const metered = await readFile(ledger, 'utf8').then((text) => JSON.parse(text) as { spent: number; denied: boolean },
        (error: NodeJS.ErrnoException) => { if (error.code !== 'ENOENT') throw error; return undefined; });
      if (metered) {
        result.usage.costUsd = metered.spent;
        if (metered.denied && result.answer.reason !== 'unexpected_gemini_model') {
          result.answer = { status: 'failed', reason: 'budget_exceeded', toolCalls: result.usage.toolCalls };
        }
      }
      return { ...result, files: await localSnapshot(workspace) };
    });
  } };
}
