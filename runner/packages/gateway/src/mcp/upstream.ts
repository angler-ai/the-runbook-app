import { pinnedFetch } from '../egress/fetch.ts';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { SSEClientTransport } from '@modelcontextprotocol/sdk/client/sse.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import type { Tool } from '@modelcontextprotocol/sdk/types.js';

export async function connectUpstream(url: string, kind: 'streamable-http' | 'sse', authorize: () => Promise<{ headers: Record<string, string>; signal?: AbortSignal }>, signal?: AbortSignal) {
  const endpoint = new URL(url);
  if (endpoint.protocol !== 'https:' || endpoint.username || endpoint.password || endpoint.hash) {
    throw new Error('invalid MCP endpoint');
  }
  const guardedFetch: typeof fetch = async (input, init) => {
    const destination = new URL(input instanceof Request ? input.url : input);
    if (destination.origin !== endpoint.origin) throw new Error('invalid MCP destination');
    const outbound = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
    const access = await authorize();
    for (const [name, value] of Object.entries(access.headers)) outbound.set(name, value);
    const signals = [signal, init?.signal, access.signal].filter((item) => item != null);
    const abort = AbortSignal.any(signals);
    abort.throwIfAborted();
    return pinnedFetch(input, { ...init, headers: outbound, redirect: 'error', signal: abort });
  };
  const transport = kind === 'sse'
    ? new SSEClientTransport(endpoint, { fetch: guardedFetch, eventSourceInit: { fetch: guardedFetch } })
    : new StreamableHTTPClientTransport(endpoint, { fetch: guardedFetch });
  const client = new Client({ name: 'runbook-gateway', version: '1' });
  try {
    await client.connect(transport, { timeout: 10_000, ...(signal && { signal }) });
  } catch {
    await client.close();
    throw new Error('MCP connection failed');
  }
  return {
    async list(): Promise<Tool[]> {
      const tools: Tool[] = [];
      let cursor: string | undefined;
      const seen = new Set<string>();
      do {
        const result = await client.listTools(cursor === undefined ? {} : { cursor }, { timeout: 10_000, ...(signal && { signal }) });
        tools.push(...result.tools);
        cursor = result.nextCursor;
        if (cursor !== undefined) {
          if (seen.has(cursor) || seen.size >= 100) throw new Error('invalid MCP pagination');
          seen.add(cursor);
        }
      } while (cursor !== undefined);
      return tools;
    },
    call: (name: string, args: Record<string, unknown>, idempotencyKey?: string) =>
      client.callTool({ name, arguments: args, ...(idempotencyKey && { _meta: { 'runbook/idempotency-key': idempotencyKey } }) }, undefined, { timeout: 30_000, ...(signal && { signal }) }),
    close: () => client.close(),
  };
}
