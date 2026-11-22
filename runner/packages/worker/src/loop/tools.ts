import { createHash } from 'node:crypto';
import type { ExecRequest, ExecResponse } from '@runbook/runner-shared/contracts/internal';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { InternalError, type workerClients } from '../clients.ts';
import { StepStopped } from './limits.ts';
import type { ExecutionContext } from '../claim.ts';

export interface AgentTool {
  parallel?: boolean;
  timeoutMs?(args: Record<string, unknown>): number;
  source?: string;
  name: string;
  description: string;
  schema: unknown;
  call(args: Record<string, unknown>, signal: AbortSignal, callId?: string): Promise<unknown>;
}

type Clients = ReturnType<typeof workerClients>;

const arg = (args: Record<string, unknown>, name: string) => {
  const value = args[name];
  if (typeof value !== 'string') throw new Error('invalid tool argument');
  return value;
};
const stringSchema = (keys: readonly string[]) => ({
  type: 'object', properties: Object.fromEntries(keys.map((key) => [key, { type: 'string' }])),
  required: keys, additionalProperties: false,
});

/** Trusted worker calls. The loop redacts and records the returned data before model delivery. */
export function runbookTools(clients: Clients, ctx: ExecutionContext): AgentTool[] {
  const read = (action: 'run' | 'step' | 'runs' | 'past-run' | 'memory', signal: AbortSignal,
    query?: Record<string, string>) => clients.context(ctx.stepRunId, ctx.claim, action, signal, query);
  const search = (args: Record<string, unknown>) => {
    const q = args.q === undefined ? '' : arg(args, 'q');
    if (Buffer.byteLength(q) > 512) throw new Error('search query too long');
    return { q };
  };
  const optionalQuery = { type: 'object', properties: { q: { type: 'string' } }, additionalProperties: false };
  let proposals = 0;
  const tools: AgentTool[] = [{
    name: 'run_overview', description: 'Read this run’s title, runbook version, origin, inputs, steps and approvals.',
    schema: stringSchema([]), call: (_args, signal) => read('run', signal),
  }, {
    name: 'step_details', description: 'Read outputs, bounded logs and evidence metadata for a completed step in this run.',
    schema: stringSchema(['step_run_id']),
    call: (args, signal) => read('step', signal, { step_run_id: arg(args, 'step_run_id') }),
  }, {
    name: 'search_runs', description: 'Find earlier runs of this runbook in this project, newest first. Optionally search by title or tag.',
    schema: optionalQuery, call: (args, signal) => read('runs', signal, search(args)),
  }, {
    name: 'get_run', description: 'Read the overview of an earlier run found by search_runs.',
    schema: stringSchema(['run_id']), call: (args, signal) => read('past-run', signal, { run_id: arg(args, 'run_id') }),
  }, {
    name: 'search_memory', description: 'Search active project and runbook memory. The query is optional.',
    schema: optionalQuery, call: (args, signal) => read('memory', signal, search(args)),
  }, {
    name: 'propose_memory', description: 'Propose a durable, evidence-backed fact for human review. Up to five per step.',
    schema: stringSchema(['title', 'body_markdown']),
    async call(args, signal) {
      const title = arg(args, 'title');
      const body_markdown = arg(args, 'body_markdown');
      if (Buffer.byteLength(title) > 1024 || Buffer.byteLength(body_markdown) > 8192) throw new Error('memory proposal too large');
      if (proposals >= 5) return { error: 'memory.proposal_limit' };
      proposals++;
      return clients.context(ctx.stepRunId, ctx.claim, 'memory', signal, {}, { title, body_markdown, tags: [] });
    },
  }];
  return tools.map((tool) => ({ ...tool, source: 'Runbook', parallel: tool.name !== 'propose_memory' }));
}

const SEARCH = String.raw`
const fs = require('node:fs');
const [operation, pattern] = process.argv.slice(1);
const escape = (text) => Array.from(text, (c) => '\\^$+?.()|{}[]'.includes(c) ? '\\' + c : c).join('');
const glob = operation === 'glob' ? new RegExp('^' + pattern.split('**').map((part) =>
  part.split('*').map(escape).join('[^/]*')).join('.*') + '$') : null;
const matches = [];
let visited = 0;
function scan(directory, prefix = '') {
  for (const item of fs.readdirSync(directory, {withFileTypes: true})) {
    if (++visited > 10000 || matches.length >= 200) return;
    const rel = prefix + item.name;
    const path = directory + '/' + item.name;
    if (item.isDirectory()) { scan(path, rel + '/'); continue; }
    if (!item.isFile() || rel === '.runbook-ca.pem') continue;
    if (glob) { if (glob.test(rel)) matches.push(rel); continue; }
    try {
      const fd = fs.openSync(path, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
      try {
        const stat = fs.fstatSync(fd);
        if (!stat.isFile() || stat.size > 65536) continue;
        const buf = Buffer.alloc(stat.size);
        const size = fs.readSync(fd, buf, 0, buf.length, 0);
        const lines = buf.subarray(0, size).toString('utf8').split('\n');
        for (const [index, line] of lines.entries()) {
          if (matches.length === 200) break;
          if (line.includes(pattern)) matches.push(rel + ':' + (index + 1) + ':' + line.slice(0, 500));
        }
      } finally { fs.closeSync(fd); }
    } catch {}
  }
}
scan('/workspace');
process.stdout.write(matches.join('\n'));
`;

export function workspaceTools(clients: Clients, ctx: ExecutionContext, id: string, timeoutMs: number, sanitizeRead?: (content: string) => Promise<unknown>): AgentTool[] {
  const base = `/internal/sandboxes/${id}`;
  function filePath(path: string) {
    const parts = path.replace(/^\/workspace\//, '').split('/');
    if (parts.some((part) => !part || part === '.' || part === '..' || /[\\\0]/.test(part))) throw new Error('invalid workspace path');
    return `${base}/files/${parts.map(encodeURIComponent).join('/')}`;
  }
  async function sandboxRequest<T>(path: string, options: Parameters<Clients['request']>[2]) {
    try {
      return await clients.request<T>('sandbox-ctl', path, options);
    } catch (error) {
      if (error instanceof InternalError && error.code === 'sandbox.gone') throw new StepStopped('sandbox_gone');
      throw error;
    }
  }
  const exec = (argv: string[], signal: AbortSignal) => sandboxRequest<ExecResponse>(`${base}/exec`, {
    body: { argv, cwd: '/workspace', timeout_s: Math.min(timeoutMs / 1000, 300) } satisfies ExecRequest,
    capability: ctx.capability(), signal, timeoutMs: timeoutMs + 1000,
  });
  const read = (path: string, signal: AbortSignal) => sandboxRequest<{ content: string; redacted?: boolean }>(filePath(path), {
    method: 'GET', capability: ctx.capability(), signal,
  });
  const write = (path: string, content: string, signal: AbortSignal) => sandboxRequest<void>(filePath(path), {
    method: 'PUT', body: { content }, capability: ctx.capability(), signal,
  });
  return [{
    name: 'shell', description: 'Run a shell command in /workspace',
    schema: stringSchema(['command']), call: (args, signal) => exec(['/bin/sh', '-c', arg(args, 'command')], signal),
  }, {
    name: 'read', description: 'Read a text file from /workspace',
    schema: stringSchema(['path']),
    async call(args, signal) {
      const { content } = await read(arg(args, 'path'), signal);
      return { content };
    },
  }, {
    name: 'write', description: 'Write a text file in /workspace',
    schema: stringSchema(['path', 'content']),
    async call(args, signal) {
      await write(arg(args, 'path'), arg(args, 'content'), signal);
      return { ok: true };
    },
  }, {
    name: 'edit', description: 'Replace one exact occurrence of old_text with new_text in a workspace file',
    schema: stringSchema(['path', 'old_text', 'new_text']),
    async call(args, signal) {
      const path = arg(args, 'path');
      const from = arg(args, 'old_text');
      const to = arg(args, 'new_text');
      const { content, redacted } = await read(path, signal);
      if (redacted || (sanitizeRead && await sanitizeRead(content) !== content)) {
        throw new Error('cannot edit redacted content');
      }
      const first = content.indexOf(from);
      if (!from || first < 0 || content.indexOf(from, first + 1) >= 0) throw new Error('replacement must match once');
      await write(path, content.slice(0, first) + to + content.slice(first + from.length), signal);
      return { ok: true };
    },
  }, ...['glob', 'grep'].map((name): AgentTool => ({
    name, description: name === 'glob' ? 'List workspace paths matching a glob using * or **' : 'Find lines containing literal text',
    schema: stringSchema(['pattern']),
    async call(args, signal) {
      const pattern = arg(args, 'pattern');
      if (!pattern || pattern.length > 1024) throw new Error('invalid search pattern');
      return exec(['/usr/local/bin/node', '-e', SEARCH, name, pattern], signal);
    },
  }))];
}

export async function mcpTools(gatewayUrl: string, token: string, ctx: ExecutionContext, initialSignal = ctx.signal, reserved?: Set<string>, readOnly = false) {
  const client = new Client({ name: 'runbook-worker', version: '0.1.1' });
  const transport = new StreamableHTTPClientTransport(new URL(`/mcp/${encodeURIComponent(ctx.stepRunId)}`, gatewayUrl), {
    fetch: async (input, init) => {
      const headers = new Headers(init?.headers);
      headers.set('authorization', `Bearer ${token}`);
      headers.set('x-runbook-capability', ctx.capability());
      const signal = init?.signal ? AbortSignal.any([init.signal, ctx.signal]) : ctx.signal;
      return fetch(input, { ...init, headers, signal, redirect: 'error' });
    },
  });
  try {
    await client.connect(transport, { signal: initialSignal });
    const listed = await client.listTools(undefined, { signal: initialSignal });
    const allowed = readOnly ? listed.tools.filter((tool) => tool._meta?.['runbook/label'] === 'read') : listed.tools;
    const pinned = reserved ? [...allowed].sort((a, b) => a.name.localeCompare(b.name, 'en')) : allowed;
    const tools: AgentTool[] = pinned.map((tool) => {
      let name = tool.name;
      if (reserved) {
        name = tool.name.replace(/[^A-Za-z0-9_-]/g, '_').slice(0, 64);
        let collision = 0;
        while (reserved.has(name)) {
          const suffix = createHash('sha256').update(`${tool.name}:${++collision}`).digest('hex').slice(0, 10);
          name = name.slice(0, 53) + '_' + suffix;
        }
        reserved.add(name);
      }
      return {
        name, description: tool.description ?? '', schema: tool.inputSchema,
        source: `MCP ${tool.name}`, parallel: tool._meta?.['runbook/label'] === 'read',
        call: (args, signal, callId) => client.callTool({
          name: tool.name, arguments: args,
          ...(callId && { _meta: { 'runbook/idempotency-key': `${ctx.stepRunId}:${callId}` } }),
        }, undefined, { signal }),
      };
    });
    return { tools, close: () => client.close() };
  } catch (error) {
    await client.close().catch(() => undefined);
    throw error;
  }
}
