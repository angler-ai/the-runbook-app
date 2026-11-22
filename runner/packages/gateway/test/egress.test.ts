import { createHash } from 'node:crypto';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import { sandboxRoute } from '../src/egress/route.ts';
import { connect } from 'node:net';
import { expect, it, vi } from 'vitest';
import { checkAuthority, checkMethod, pinnedAddress, policyForHost } from '../src/egress/rules.ts';
import { injectHeader } from '../src/egress/inject.ts';
import { responseGuard } from '../src/egress/guard.ts';
import { MaterialStore } from '../src/material.ts';
import { testConnectionRoute } from '../src/egress/test-connection.ts';
import { ListenerSecret, sandboxListener } from '../src/egress/listener.ts';

const localFetch = fetch;
const host = 'api.example';
const rules = { [host]: 'read' as const };

it('unknown_host_refused', () => {
  expect(() => policyForHost('other.example', [rules, rules, rules], [host])).toThrow('egress denied');
});
it('resolves_and_pins_ip_and_refuses_private_ranges', async () => {
  await expect(pinnedAddress(host, async () => [{ address: '8.8.8.8', family: 4 }])).resolves.toEqual({
    address: '8.8.8.8', family: 4,
  });
  for (const address of ['10.0.0.5', '169.254.169.254', '127.0.0.1', '100.64.0.1']) {
    await expect(pinnedAddress(host, async () => [{ address, family: 4 }])).rejects.toThrow('egress denied');
  }
});
it('host_sni_connect_mismatch_refused', () => {
  expect(() => checkAuthority(host, 'other.example', host)).toThrow('egress denied');
  expect(() => checkAuthority(host, host, 'other.example')).toThrow('egress denied');
  expect(() => checkAuthority(host, host, host + ':443')).not.toThrow();
});
it('read_host_allows_only_get_head_options', () => {
  for (const method of ['GET', 'HEAD', 'OPTIONS']) expect(() => checkMethod('read', method)).not.toThrow();
  expect(() => checkMethod('read', 'POST')).toThrow('egress denied');
});
it('header_set_overwrites_sandbox_value', () => {
  const headers = new Headers({ 'x-api-key': 'fake' });
  injectHeader(headers, { header_name: 'X-API-Key', header_format: 'Bearer {}' }, 'secret');
  expect(Object.fromEntries(headers)).toEqual({ 'x-api-key': 'Bearer secret' });
});
it('strips range headers when injecting a credential', () => {
  const headers = new Headers({ Range: 'bytes=0-4', 'If-Range': '"etag"' });
  injectHeader(headers, { header_name: 'x-api-key', header_format: '{}' }, 'secret');
  expect(Object.fromEntries(headers)).toEqual({ 'x-api-key': 'secret' });
});
it('no_body_substitution', () => {
  const body = Buffer.from('placeholder');
  const headers = new Headers();
  injectHeader(headers, { header_name: 'x-api-key', header_format: '{}' }, 'secret');
  expect(body.toString()).toBe('placeholder');
});
it('listener_secret_scoped_to_sandbox', () => {
  const a = new ListenerSecret();
  const b = new ListenerSecret();
  expect(a.accepts(a.header)).toBe(true);
  expect(b.accepts(a.header)).toBe(false);
});
it('metadata_probe_through_gateway_fails', async () => {
  const listener = await sandboxListener({
    claims: {
      destructive: false, network: null, model_profile: null, max_tokens: null, timeout_ms: null, step_deadline_ms: null, connections: [],
      iss: 'https://core.example', aud: 'runner:r', sub: 'step', kind: 'capability', claim_id: 'claim',
      generation: 1, runner_id: 'r', group_id: 'g', project_id: 'p', agent_id: 'a', agent_version: 'v',
      iat: Math.floor(Date.now() / 1000), exp: Math.floor(Date.now() / 1000) + 60, jti: 'capability',
    },
    store: new MaterialStore(), layers: [rules, rules, rules], listenHost: '127.0.0.1', publicHost: '127.0.0.1',
  });
  const address = new URL(listener.proxy_url);
  try {
    const response = await new Promise<string>((resolve, reject) => {
      const socket = connect(Number(address.port), address.hostname, () => {
        const token = Buffer.from(`runbook:${listener.listener_secret}`).toString('base64');
        socket.write(`CONNECT 169.254.169.254:443 HTTP/1.1\r\nHost: 169.254.169.254:443\r\nProxy-Authorization: Basic ${token}\r\n\r\n`);
      });
      let body = '';
      socket.setTimeout(3000, () => socket.destroy(new Error('proxy timed out')));
      socket.setEncoding('utf8');
      socket.on('data', (chunk: string) => { body += chunk; });
      socket.once('end', () => resolve(body));
      socket.once('error', reject);
    });
    expect(response).toMatch(/^HTTP\/1.1 403 /);
  } finally {
    listener.close();
  }
});
it('response_echo_redacted', () => {
  expect(responseGuard(['secret'])(Buffer.from('echo secret')).toString()).toBe('echo [REDACTED]');
});

it('connection_test_refuses_an_unbound_destination_without_sending', async () => {
  const store = new MaterialStore();
  const release_id = 'test-release';
  store.put({
    release_id,
    step_run_id: `test:${release_id}`,
    credential_id: 'secret-id',
    delivery: 'proxy',
    binding: { hosts: [host], test: { method: 'GET', path: '/' }, header_name: 'x-api-key', header_format: '{}' },
    material: { fields: { key: 'test-secret' } },
    expires_at: new Date(Date.now() + 60_000).toISOString(),
  });
  const send = vi.fn(async () => new Response('test-secret', { status: 200 }));
  const route = testConnectionRoute(store, send);
  try {
    const test = (url: string) => route.handle({
      caller: 'vault', params: {}, body: { release_id, request: { url, method: 'GET' } },
    });
    expect(await test('https://unbound.example/')).toEqual({ body: { outcome: 'unreachable' } });
    expect(send).not.toHaveBeenCalled();
    expect(await test(`https://${host}/`)).toEqual({ body: { outcome: 'response', status: 200 } });
    expect(send.mock.calls).toHaveLength(1);
  } finally {
    store.clear();
  }
});

it('fences sandbox IDs and renews a listener without replacing its CA, then tears it down', async () => {
  vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] });
  const start = Date.now();
  const claims = { runner_id: 'runner', sub: 'step', claim_id: 'claim', generation: 1,
    exp: Math.floor(start / 1000) + 600 } as GrantClaims;
  const sandbox_id = 'rb-' + createHash('sha256').update(JSON.stringify(['runner', 'step', 'claim', 1])).digest('hex');
  const config = { role: 'gateway' as const, urls: { api: 'https://api.example' }, outboundTokens: { api: 'token' },
    inboundTokens: {}, listen: { host: '127.0.0.1', port: 0 } };
  const verifier = { verifyCapability: async (token: string) => token === 'renewed'
    ? { ...claims, exp: claims.exp + 300 } : token === 'other' ? { ...claims, generation: 2 } : claims };
  vi.stubGlobal('fetch', async () => Response.json({ layers: [rules, rules, rules], destructive: false, credentialLabels: {} }));
  const listeners = sandboxRoute(config, verifier, new MaterialStore(), '127.0.0.1');
  const call = (method: string, capability: string, id = sandbox_id) => {
    const route = listeners.routes.find((r) => r.method === method && (method !== 'POST' || r.path.endsWith('/renew'))) !;
    return route.handle({ caller: 'sandbox-ctl', params: { sandbox_id: id }, body: { capability, sandbox_id: id } });
  };
  const create = (id: string) => listeners.routes[0]!.handle({
    caller: 'sandbox-ctl', params: {}, body: { capability: 'original', sandbox_id: id },
  });
  try {
    await expect(create('arbitrary-id')).rejects.toMatchObject({ status: 403 });
    const created = await create(sandbox_id);
    const body = created.body as { proxy_url: string; ca_pem: string; listener_secret: string };
    await expect(call('POST', 'other')).rejects.toMatchObject({ status: 403 });
    vi.advanceTimersByTime(300_000);
    expect(await call('POST', 'renewed')).toEqual({ body });
    vi.advanceTimersByTime(301_000);
    expect((await localFetch(body.proxy_url)).status).toBe(405);
    expect(await call('DELETE', 'renewed')).toEqual({ status: 204 });
    await expect(localFetch(body.proxy_url)).rejects.toThrow();
  } finally {
    listeners.close();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  }
});

it('redacts encoded credential echoes from upstream', () => {
  const secret = 'test token "+/?=🚀';
  const encoded = [
    Buffer.from(secret).toString('base64'),
    Buffer.from(secret).toString('base64url'),
    Buffer.from(secret).toString('hex'),
    encodeURIComponent(secret),
    JSON.stringify(secret).slice(1, -1),
  ];
  const guard = responseGuard([secret]);
  for (const value of encoded) expect(guard(Buffer.from(value)).toString()).toBe('[REDACTED]');
});
