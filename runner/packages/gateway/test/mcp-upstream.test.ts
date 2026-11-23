import { expect, it, vi } from 'vitest';
import { pinnedFetch } from '../src/egress/fetch.ts';
import { connectUpstream } from '../src/mcp/upstream.ts';
import { StepTools } from '../src/mcp/snapshot.ts';
import { MaterialStore } from '../src/material.ts';

vi.mock('../src/egress/fetch.ts', () => ({ pinnedFetch: vi.fn() }));

it('cancels the authenticated transport during refresh without dispatching tools/call', async () => {
  vi.useFakeTimers();
  const store = new MaterialStore(() => Date.now());
  const endpoint = 'https://fixture.example/mcp';
  const release = { release_id: 'release', step_run_id: 'step', credential_id: 'cred', delivery: 'mcp' as const,
    expires_at: new Date(Date.now() + 60_000).toISOString(), binding: { url: endpoint }, material: { fields: { token: 'fixture-token' } } };
  store.put(release);
  const tool = { name: 'read', inputSchema: { type: 'object' as const } };
  const requests: string[] = [];
  const authorize = vi.fn(async () => {
    const live = store.get(release.release_id);
    if (!live) throw new Error('release unavailable');
    return { headers: { authorization: `Bearer ${live.material.fields.token}` }, signal: store.signal(live) };
  });
  vi.mocked(pinnedFetch).mockImplementation(async (_input, init) => {
    const request = JSON.parse(String(init?.body)) as { method: string; id?: number };
    requests.push(request.method);
    expect(new Headers(init?.headers).get('authorization')).toBe('Bearer fixture-token');
    if (request.method === 'initialize') {
      return Response.json({ jsonrpc: '2.0', id: request.id, result: {
        protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1' },
      } });
    }
    if (request.method === 'notifications/initialized') return new Response(null, { status: 202 });
    if (request.method === 'tools/list') {
      setTimeout(() => store.revokeCredential('cred'), 5);
      await vi.advanceTimersByTimeAsync(5);
      expect(init?.signal?.aborted).toBe(true);
      // Simulate a response that reaches the transport despite cancellation.
      return Response.json({ jsonrpc: '2.0', id: request.id, result: { tools: [tool] } });
    }
    throw new Error('unexpected outbound request');
  });

  try {
    const upstream = await connectUpstream(endpoint, 'streamable-http', authorize);
    try {
      const tools = new StepTools([{ tool, label: 'read' }], upstream, false);
      await expect(tools.call('read', {})).rejects.toThrow();
      expect(requests).toEqual(['initialize', 'notifications/initialized', 'tools/list']);
      expect(authorize).toHaveBeenCalledTimes(5);
    } finally {
      await upstream.close();
    }
  } finally {
    store.clear();
    vi.useRealTimers();
  }
});

it('never authorizes a negotiated SSE message endpoint in another tenant', async () => {
  vi.mocked(pinnedFetch).mockReset();
  vi.mocked(pinnedFetch).mockResolvedValueOnce(new Response(
    'event: endpoint\ndata: /tenant-b/mcp?sessionId=fixture\n\n',
    { headers: { 'content-type': 'text/event-stream' } },
  ));
  const authorize = vi.fn(async () => ({ headers: { authorization: 'Bearer tenant-a-token' } }));
  await expect(connectUpstream('https://fixture.example/tenant-a/mcp', 'sse', authorize)).rejects.toThrow('MCP connection failed');
  expect(authorize).toHaveBeenCalledTimes(1);
  expect(pinnedFetch).toHaveBeenCalledTimes(1);
});

it.each([
  [401, 'Bearer error="invalid_token"', 'missing_access'],
  [403, 'Bearer error="insufficient_scope"', 'insufficient_scope'],
] as const)('returns a structured blocked outcome on HTTP %s without replaying the tool call', async (status, challenge, kind) => {
  vi.mocked(pinnedFetch).mockReset();
  const requests: string[] = [];
  vi.mocked(pinnedFetch).mockImplementation(async (_input, init) => {
    const payload = JSON.parse(String(init?.body)) as { method: string; id?: number };
    requests.push(payload.method);
    if (payload.method === 'initialize') return Response.json({ jsonrpc: '2.0', id: payload.id, result: {
      protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1' },
    } });
    if (payload.method === 'notifications/initialized') return new Response(null, { status: 202 });
    return new Response(null, { status, headers: { 'www-authenticate': challenge } });
  });
  const upstream = await connectUpstream('https://fixture.example/tenant-a/mcp', 'streamable-http', async () => ({
    headers: { authorization: 'Bearer fixture' }, oauth: { release_id: 'release', token_version: 2 },
  }));
  try {
    await expect(upstream.call('write', {})).rejects.toMatchObject({ code: -32040, data: { kind } });
    expect(requests.filter((method) => method === 'tools/call')).toEqual(['tools/call']);
  } finally {
    await upstream.close();
  }
});
