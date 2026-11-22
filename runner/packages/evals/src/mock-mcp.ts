import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { ListToolsRequestSchema, CallToolRequestSchema } from '@modelcontextprotocol/sdk/types.js';
import { runbookTools } from '@runbook/runner-worker/loop/tools';
import type { workerClients } from '@runbook/runner-worker/clients';
import { isDeepStrictEqual } from 'node:util';
import type { Task } from './tasks.ts';

const schema = { type: 'object' as const, properties: {}, additionalProperties: false };

function toolSchema(args?: Record<string, string>) {
  if (!args) return schema;
  return { ...schema, properties: Object.fromEntries(Object.keys(args).map((key) => [key, { type: 'string' }])),
    required: Object.keys(args) };
}

export function fixtureServer(tools: Task['mcp'], read?: (name: string) => Promise<string | undefined>) {
  const server = new Server({ name: 'runbook-eval-fixture', version: '1.0.0' }, { capabilities: { tools: {} } });
  server.setRequestHandler(ListToolsRequestSchema, () => ({
    tools: Object.entries(tools).map(([name, { description }]) => ({ name, description, inputSchema: toolSchema(tools[name]?.arguments) })),
  }));
  server.setRequestHandler(CallToolRequestSchema, async ({ params: { name, arguments: args } }) => {
    if (!Object.hasOwn(tools, name) || !isDeepStrictEqual(args ?? {}, tools[name]?.arguments ?? {})) {
      return { isError: true, content: [{ type: 'text', text: 'Unknown fixture tool or unexpected arguments.' }] };
    }
    const { data, unchanged } = tools[name]!;
    for (const [file, content] of Object.entries(unchanged ?? {})) {
      if (!read || await read(file) !== content) {
        return { isError: true, content: [{ type: 'text', text: 'The workspace changed before the precondition check.' }] };
      }
    }
    const isError = Boolean(data && typeof data === 'object' && Object.hasOwn(data, 'error'));
    return { isError, content: [{ type: 'text', text: JSON.stringify(data) }] };
  });
  return server;
}

export async function connectFixtures(task: Task, signal: AbortSignal, read?: (name: string) => Promise<string | undefined>) {
  const server = fixtureServer(task.mcp, read);
  const client = new Client({ name: 'runbook-eval-worker', version: '1.0.0' });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  const close = async () => { await client.close(); await server.close(); };
  try {
    await server.connect(serverTransport);
    await client.connect(clientTransport, { signal });
    const listed = await client.listTools(undefined, { signal });
    return { close, tools: listed.tools.map(({ name, description, inputSchema }) => ({
      name, description: description ?? '', schema: inputSchema,
      call: (args: Record<string, unknown>, callSignal: AbortSignal) =>
        client.callTool({ name, arguments: args }, undefined, { signal: callSignal }),
    })) };
  } catch (error) {
    await close();
    throw error;
  }
}

const contextTools = { run: 'run_overview', step: 'step_details', runs: 'search_runs', 'past-run': 'get_run', memory: 'search_memory' } as const;

export function fixtureRunbookTools(task: Task, signal: AbortSignal) {
  const context: ReturnType<typeof workerClients>['context'] = async <T>(_step: string, _claim: unknown,
    action: keyof typeof contextTools, callSignal: AbortSignal, args: Record<string, string> = {}) => {
    callSignal.throwIfAborted();
    const fixture = task.mcp[contextTools[action]];
    return (fixture && isDeepStrictEqual(args, fixture.arguments ?? {}) ? fixture.data : { error: 'Unexpected context lookup' }) as T;
  };
  return runbookTools({ context } as ReturnType<typeof workerClients>, {
    stepRunId: task.id, claim: { id: 'fixture', generation: 1 }, signal,
    capability: () => 'fixture', registerSandbox: () => undefined, sandboxStarted: () => undefined,
  }).filter(({ name }) => Object.hasOwn(task.mcp, name));
}
