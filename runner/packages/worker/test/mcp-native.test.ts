import { expect, it, vi } from 'vitest';
import type { ExecutionContext } from '../src/claim.ts';
import { mcpTools } from '../src/loop/tools.ts';

it('disambiguates MCP tools from built-ins and each other while preserving the original wire names', async () => {
  const inputs: string[] = [];
  const schema = { type: 'object', $schema: 'https://json-schema.org/draft/2020-12/schema', $id: 'urn:fixture:mcp' };
  const names = ['remote.tool', 'remote_tool', 'view'];
  vi.stubGlobal('fetch', async (_url: string, init: RequestInit) => {
    const request = JSON.parse(String(init.body));
    if (request.method === 'notifications/initialized') return new Response(null, { status: 202 });
    if (request.method === 'tools/call') inputs.push(request.params.name);
    const result = request.method === 'initialize' ? {
      protocolVersion: request.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1' },
    } : request.method === 'tools/list' ? {
      tools: names.map((name) => ({ name, inputSchema: schema, _meta: { 'runbook/label': 'read' } })),
    } : { content: [{ type: 'text', text: 'fixture' }] };
    return Response.json({ jsonrpc: '2.0', id: request.id, result });
  });
  const signal = new AbortController().signal;
  const ctx = { stepRunId: 'step', capability: () => 'fixture', signal } as ExecutionContext;
  let mcp: Awaited<ReturnType<typeof mcpTools>> | undefined;
  try {
    mcp = await mcpTools('http://fixture', 'fixture', ctx, signal, new Set(['view']));
    expect(new Set(mcp.tools.map((tool) => tool.name)).size).toBe(3);
    expect(mcp.tools.every((tool) => tool.name !== 'view' && /^[\w-]{1,64}$/.test(tool.name))).toBe(true);
    for (const tool of mcp.tools) {
      expect(tool.schema).toMatchObject(schema);
      await tool.call({}, signal, 'test');
    }
    expect(inputs.sort()).toEqual(names);
  } finally {
    await mcp?.close();
    vi.unstubAllGlobals();
  }
});

it('decision tooling includes only explicitly read-labeled MCP tools', async () => {
  vi.stubGlobal('fetch', async (_url: string, init: RequestInit) => {
    const request = JSON.parse(String(init.body));
    if (request.method === 'notifications/initialized') return new Response(null, { status: 202 });
    const result = request.method === 'initialize' ? {
      protocolVersion: request.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1' },
    } : {
      tools: ['read', 'write', 'destructive', 'unlabeled'].map((name) => ({
        name, inputSchema: { type: 'object' }, _meta: name === 'unlabeled' ? {} : { 'runbook/label': name },
      })),
    };
    return Response.json({ jsonrpc: '2.0', id: request.id, result });
  });
  const signal = new AbortController().signal;
  const ctx = { stepRunId: 'step', capability: () => 'fixture', signal } as ExecutionContext;
  let mcp: Awaited<ReturnType<typeof mcpTools>> | undefined;
  try {
    mcp = await mcpTools('http://fixture', 'fixture', ctx, signal, new Set(), true);
    expect(mcp.tools.map(({ name }) => name)).toEqual(['read']);
  } finally {
    await mcp?.close();
    vi.unstubAllGlobals();
  }
});
