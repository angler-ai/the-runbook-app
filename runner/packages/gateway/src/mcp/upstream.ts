import { pinnedFetch } from '../egress/fetch.ts';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { SSEClientTransport } from '@modelcontextprotocol/sdk/client/sse.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { MCP_ACCESS_BLOCKED, type McpAccessBlocked } from '@runbook/runner-shared/contracts/internal';
import { McpError, type Tool } from '@modelcontextprotocol/sdk/types.js';

function boundRequest(endpoint: URL, destination: URL, negotiated: boolean): boolean {
  if (destination.origin !== endpoint.origin || destination.username || destination.password || destination.hash) return false;
  if (destination.href === endpoint.href) return true;
  if (!negotiated) return false;

  const prefix = endpoint.pathname.endsWith('/') ? endpoint.pathname : `${endpoint.pathname}/`;
  if (destination.pathname !== endpoint.pathname && !destination.pathname.startsWith(prefix)) return false;
  // Reject path components whose interpretation may change at a reverse proxy.
  if (/[%\\;]|\/\//.test(destination.pathname)) return false;
  const session = [...destination.searchParams];
  for (const [name, value] of endpoint.searchParams) {
    const index = session.findIndex(([key, entry]) => key === name && entry === value);
    if (index === -1) return false;
    session.splice(index, 1);
  }
  return session.length === 1 && /^(sessionId|session_id)$/.test(session[0]![0]) &&
    /^[A-Za-z0-9._~-]{1,256}$/.test(session[0]![1]);
}

/** Only a locally classified HTTP denial can instruct a worker to renew. */
export class UpstreamAccessBlocked extends McpError {
  constructor(data: McpAccessBlocked) {
    super(MCP_ACCESS_BLOCKED, 'MCP credential unavailable', data);
  }
}

interface UpstreamAuthorization {
  headers: Record<string, string>;
  signal?: AbortSignal;
  oauth?: { release_id: string; token_version: number };
  accepted?: () => void;
}

export async function connectUpstream(url: string, kind: 'streamable-http' | 'sse', authorize: () => Promise<UpstreamAuthorization>, signal?: AbortSignal) {
  const endpoint = new URL(url);
  if (endpoint.protocol !== 'https:' || endpoint.username || endpoint.password || endpoint.hash) {
    throw new Error('invalid MCP endpoint');
  }
  let blocked: UpstreamAccessBlocked | undefined;
  const guardedFetch: typeof fetch = async (input, init) => {
    const destination = new URL(input instanceof Request ? input.url : input);
    const method = init?.method ?? (input instanceof Request ? input.method : 'GET');
    if (!boundRequest(endpoint, destination, kind === 'sse' && method === 'POST')) {
      throw new Error('invalid MCP destination');
    }
    const outbound = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
    const access = await authorize().catch((error: unknown) => {
      if (error instanceof UpstreamAccessBlocked) blocked = error;
      throw error;
    });
    for (const [name, value] of Object.entries(access.headers)) outbound.set(name, value);
    const signals = [signal, init?.signal, access.signal].filter((item) => item != null);
    const abort = AbortSignal.any(signals);
    abort.throwIfAborted();
    const response = await pinnedFetch(input, { ...init, headers: outbound, redirect: 'error', signal: abort });
    if (access.oauth && (response.status === 401 || response.status === 403)) {
      const challenge = response.headers.get('www-authenticate') ?? '';
      const insufficient = /(?:^|[\s,])error=(?:"insufficient_scope"|insufficient_scope)(?=[\s,]|$)/i.test(challenge);
      if (insufficient || response.status === 401) {
        const data: McpAccessBlocked = insufficient ? { kind: 'insufficient_scope' } : { kind: 'missing_access', ...access.oauth };
        blocked = new UpstreamAccessBlocked(data);
        await response.body?.cancel();
        throw blocked;
      }
    }
    if (response.ok) access.accepted?.();
    return response;
  };
  const transport = kind === 'sse'
    ? new SSEClientTransport(endpoint, { fetch: guardedFetch, eventSourceInit: { fetch: guardedFetch } })
    : new StreamableHTTPClientTransport(endpoint, { fetch: guardedFetch });
  const client = new Client({ name: 'runbook-gateway', version: '1' });
  try {
    await client.connect(transport, { timeout: 10_000, ...(signal && { signal }) });
  } catch {
    await client.close();
    throw blocked ?? new Error('MCP connection failed');
  }
  async function classified<T>(operation: Promise<T>): Promise<T> {
    try {
      return await operation;
    } catch (error) {
      throw blocked ?? error;
    }
  }
  return {
    async list(): Promise<Tool[]> {
      const tools: Tool[] = [];
      let cursor: string | undefined;
      const seen = new Set<string>();
      do {
        const result = await classified(client.listTools(cursor === undefined ? {} : { cursor }, { timeout: 10_000, ...(signal && { signal }) }));
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
      classified(client.callTool({ name, arguments: args, ...(idempotencyKey && { _meta: { 'runbook/idempotency-key': idempotencyKey } }) }, undefined, { timeout: 30_000, ...(signal && { signal }) })),
    close: () => client.close(),
  };
}
