import * as discovery from '../src/mcp/server.ts';
import { expect, it, vi } from 'vitest';
import { StepTools, upstreamHeaders } from '../src/mcp/snapshot.ts';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import type { Redaction } from '@runbook/runner-shared/redact';
import { mcpRoute } from '../src/mcp/server.ts';
import { connectUpstream } from '../src/mcp/upstream.ts';
import { MaterialStore } from '../src/material.ts';

vi.mock('../src/mcp/upstream.ts', async (load) => ({
  ...await load<Record<string, unknown>>(),
  connectUpstream: vi.fn(async (_url, _kind, authorize) => {
    await authorize();
    return {
      list: async () => [allowed],
      call: async () => ({ content: [{ type: 'text', text: 'fixture result' }] }),
      close: async () => {},
    };
  }),
}));

const schema = { type: 'object' as const };
const allowed = { name: 'read', description: 'Pinned description', inputSchema: schema };
const dangerous = { name: 'delete', inputSchema: schema };

function setup(description = allowed.description) {
  const upstream = {
    list: async () => [{ ...allowed, description }, dangerous],
    call: vi.fn(async () => ({ content: [{ type: 'text', text: 'result' }] })),
  };
  const tools = new StepTools([
    { tool: allowed, label: 'read' },
    { tool: dangerous },
  ], upstream, false);
  return { tools, upstream };
}

it('lists_only_allowed_tools', async () => {
  const { tools } = setup();
  expect(await tools.list()).toEqual([allowed]);
});
it('call_to_unlisted_tool_refused', async () => {
  const { tools, upstream } = setup();
  await expect(tools.call('unlisted', {})).rejects.toThrow('tool unavailable');
  expect(upstream.call).not.toHaveBeenCalled();
});
it('hides_tools_with_changed_descriptions', async () => {
  const { tools } = setup('Untrusted description');
  expect(await tools.list()).toEqual([]);
});
it('changed_tool_disabled_until_repinned', async () => {
  const { tools, upstream } = setup('Changed');
  await expect(tools.call('read', {})).rejects.toThrow('tool unavailable');
  expect(upstream.call).not.toHaveBeenCalled();
});
it('destructive_tool_refused_in_non_destructive_step', async () => {
  const { tools, upstream } = setup();
  await expect(tools.call('delete', {})).rejects.toThrow('tool unavailable');
  expect(upstream.call).not.toHaveBeenCalled();
});
it('auth_header_added_upstream_only', () => {
  const inbound = new Headers({ authorization: 'Bearer process' });
  expect(upstreamHeaders('https://mcp.example', {
    url: 'https://mcp.example', token: 'credential',
  })).toEqual({ authorization: 'Bearer credential' });
  expect(inbound.get('authorization')).toBe('Bearer process');
  expect(() => upstreamHeaders('https://other.example', {
    url: 'https://mcp.example', token: 'credential',
  })).toThrow('tool unavailable');
});

it('posts redacted tool records to API instead of exposing them to the worker', async () => {
  const now = Math.floor(Date.now() / 1000);
  const claims = { sub: 'step', kind: 'capability', runner_id: 'r', claim_id: 'claim', generation: 1, exp: now + 60 } as GrantClaims;
  const verifier = { verifyCapability: async () => claims };
  const store = new MaterialStore();
  const secret = 'fixture-live-secret';
  store.put({ release_id: 'r', step_run_id: 'step', claim_id: 'claim', generation: 1, runner_id: 'r', credential_id: 'c',
    delivery: 'proxy', expires_at: new Date((now + 60) * 1000).toISOString(), binding: {}, material: { fields: { key: secret } } });
  const fetcher = vi.fn(async (url: string, _init: RequestInit) => url.endsWith('/snapshot')
    ? Response.json({ destructive: false, servers: [{ id: 's', url: 'https://fixture.example', transport: 'streamable-http',
      tools: [{ tool: allowed, label: 'read' }] }] })
    : new Response(null, { status: 204 }));
  vi.stubGlobal('fetch', fetcher);
  const route = mcpRoute({ role: 'gateway', urls: { api: 'https://api.example' }, outboundTokens: { api: 'gateway-token' },
    inboundTokens: {}, listen: { host: '127.0.0.1', port: 0 } }, verifier, store,
  { redact: (value: string, secrets: string[]) => secrets.reduce((text, s) => text.replaceAll(s, '[REDACTED]'), value) } as Redaction);
  try {
    await expect(route.handle({ caller: 'worker', runnerId: 'other', params: { step_run_id: 'step' }, body: undefined,
      request: { method: 'POST', headers: { 'x-runbook-capability': 'test' } } }))
      .rejects.toMatchObject({ code: 'mcp.runner_denied' });
    expect(fetcher).not.toHaveBeenCalled();
    const reply = await route.handle({
      caller: 'worker', runnerId: 'r', params: { step_run_id: 'step' },
      request: { method: 'POST', headers: { accept: 'application/json, text/event-stream', 'content-type': 'application/json',
        'x-runbook-capability': 'test' } },
      body: { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 's__read', arguments: { key: secret } } },
    });
    expect(reply.headers).toBeUndefined();
    const sent = fetcher.mock.calls.find(([url]) => url.endsWith('/tool-calls'));
    expect(sent?.[1].headers).toMatchObject({ authorization: 'Bearer gateway-token' });
    expect(JSON.parse(String(sent?.[1].body))).toMatchObject({
      tool: 'read', server_id: 's', args_redacted: { key: '[REDACTED]' }, duration_ms: expect.any(Number),
      args_bytes: expect.any(Number), result_bytes: expect.any(Number),
    });
  } finally {
    store.clear();
    vi.unstubAllGlobals();
  }
});

it('discovers tools with a bound test release and refuses a different destination', async () => {
  const store = new MaterialStore();
  const releaseId = 'discovery';
  store.put({ release_id: releaseId, step_run_id: `test:${releaseId}`, credential_id: 'cred', delivery: 'mcp',
    expires_at: new Date(Date.now() + 60_000).toISOString(), binding: { url: 'https://fixture.example/mcp' },
    material: { fields: { token: 'secret' } } });
  const route = discovery.discoveryRoute(store, { redact: (value: string) => value } as Redaction);
  const call = (url: string) => route.handle({ caller: 'vault', params: {},
    body: { url, transport: 'streamable-http', release_id: releaseId } });
  try {
    expect(await call('https://fixture.example/mcp')).toEqual({ body: { tools: [allowed] } });
    await expect(call('https://other.example/mcp')).rejects.toMatchObject({ status: 502 });
  } finally {
    store.clear();
  }
});

it('aborts the release during catalog refresh and refuses tools/call', async () => {
  const now = Date.now();
  const claims = { sub: 'step', kind: 'capability', runner_id: 'r', claim_id: 'claim', generation: 1,
    exp: Math.floor(now / 1000) + 60 } as GrantClaims;
  const verifyCapability = vi.fn(async () => claims);
  const store = new MaterialStore(() => Date.now());
  const url = 'https://fixture.example/mcp';
  store.put({ release_id: 'release', step_run_id: 'step', claim_id: 'claim', generation: 1, runner_id: 'r', credential_id: 'cred',
    delivery: 'mcp', expires_at: new Date(now + 60_000).toISOString(), binding: { url }, material: { fields: { token: 'secret' } } });
  const call = vi.fn(async () => ({ content: [] }));
  let upstreamSignal: AbortSignal | undefined;
  vi.mocked(connectUpstream).mockImplementationOnce(async (_url, _kind, headers) => {
    const authorize = async () => {
      if (typeof headers !== 'function') return;
      upstreamSignal = (await headers()).signal;
      upstreamSignal?.throwIfAborted();
    };
    await authorize();
    return {
      async list() {
        await authorize();
        // Revoke after authorization and before returning the refreshed catalog.
        store.revokeCredential('cred');
        return [allowed];
      },
      async call() { await authorize(); return call(); },
      async close() {},
    };
  });
  vi.stubGlobal('fetch', vi.fn(async (target: string) => target.endsWith('/snapshot')
    ? Response.json({ destructive: false, credentialLabels: { cred: 'read' }, servers: [
      { id: 's', url, transport: 'streamable-http', credential_id: 'cred', tools: [{ tool: allowed, label: 'read' }] },
    ] })
    : new Response(null, { status: 204 })));
  try {
    const route = mcpRoute({ role: 'gateway', urls: { api: 'https://api.example' }, outboundTokens: { api: 'gateway-token' },
      inboundTokens: {}, listen: { host: '127.0.0.1', port: 0 } }, { verifyCapability }, store,
    { redact: (value: string) => value } as Redaction);
    const reply = await route.handle({
      caller: 'worker', runnerId: 'r', params: { step_run_id: 'step' },
      request: { method: 'POST', headers: { accept: 'application/json, text/event-stream', 'content-type': 'application/json',
        'x-runbook-capability': 'test' } },
      body: { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 's__read' } },
    });
    expect(reply.body).toMatchObject({ result: { isError: true } });
    expect(upstreamSignal?.aborted).toBe(true);
    expect(call).not.toHaveBeenCalled();
    expect(verifyCapability).toHaveBeenCalledTimes(4);
  } finally {
    store.clear();
    vi.unstubAllGlobals();
  }
});

it('decision grant refuses write tools and write or unbound MCP credentials even if the snapshot offers them', async () => {
  const claims = { sub: 'step', kind: 'capability', runner_id: 'r', claim_id: 'claim', generation: 1,
    exp: Math.floor(Date.now() / 1000) + 60, reach: 'read', connections: ['writer'] } as GrantClaims;
  const store = new MaterialStore();
  vi.stubGlobal('fetch', vi.fn(async (target: string) => target.endsWith('/snapshot')
    ? Response.json({ destructive: true, credentialLabels: { writer: 'write', unbound: 'read' },
      servers: [
        { id: 'toolwrite', url: 'https://fixture.example', transport: 'streamable-http', tools: [{ tool: allowed, label: 'write' }] },
        { id: 'credwrite', url: 'https://fixture.example', transport: 'streamable-http', credential_id: 'writer', tools: [{ tool: allowed, label: 'read' }] },
        { id: 'unbound', url: 'https://fixture.example', transport: 'streamable-http', credential_id: 'unbound', tools: [{ tool: allowed, label: 'read' }] },
        { id: 'read', url: 'https://fixture.example', transport: 'streamable-http', tools: [{ tool: allowed, label: 'read' }] },
      ] })
    : new Response(null, { status: 204 })));
  try {
    const route = mcpRoute({ role: 'gateway', urls: { api: 'https://api.example' }, outboundTokens: { api: 'gateway-token' },
      inboundTokens: {}, listen: { host: '127.0.0.1', port: 0 } }, { verifyCapability: async () => claims }, store,
    { redact: (value: string) => value } as Redaction);
    const request = (method: string, params?: { name: string }) => route.handle({
      caller: 'worker', runnerId: 'r', params: { step_run_id: 'step' },
      request: { method: 'POST', headers: { accept: 'application/json, text/event-stream', 'content-type': 'application/json',
        'x-runbook-capability': 'test' } },
      body: { jsonrpc: '2.0', id: 1, method, params },
    });
    expect((await request('tools/list')).body).toMatchObject({ result: { tools: [{ name: 'read__read' }] } });
    for (const server of ['toolwrite', 'credwrite', 'unbound']) {
      expect((await request('tools/call', { name: `${server}__read` })).body).toMatchObject({ result: { isError: true } });
    }
    expect((await request('tools/call', { name: 'read__read' })).body).toMatchObject({ result: {
      content: [{ type: 'text', text: 'fixture result' }],
    } });
  } finally {
    store.clear();
    vi.unstubAllGlobals();
  }
});
