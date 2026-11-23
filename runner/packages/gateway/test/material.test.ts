import type { AddressInfo } from 'node:net';
import { createGatewayServer } from '../src/server.ts';
import { afterEach, expect, it, vi } from 'vitest';
import { MaterialStore } from '../src/material.ts';
import { CapabilityVerifier } from '../src/capability.ts';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import { createLogger } from '@runbook/runner-shared/log';
import { loadConfig } from '@runbook/runner-shared/config';
import { generateSigningKey, signJwt } from '../../shared/test-utils/jwt.ts';

const release = {
  release_id: 'release', step_run_id: 'step', claim_id: 'claim', generation: 1, runner_id: 'runner',
  credential_id: 'credential', delivery: 'proxy' as const, binding: {},
  material: { fields: { key: 'fixture-secret' } }, expires_at: new Date(10_000).toISOString(),
};
const claims = {
  destructive: false, network: null, model_profile: null, max_tokens: null, timeout_ms: null, step_deadline_ms: null, connections: ['credential'],
  sub: 'step', aud: 'runner:runner', iss: 'https://core.example', exp: 10, iat: 0,
  kind: 'capability', claim_id: 'claim', generation: 1, runner_id: 'runner',
  group_id: 'group', project_id: 'project', agent_id: 'agent', agent_version: 'version', jti: 'token-id',
};

afterEach(() => vi.useRealTimers());

it('material_dropped_at_expiry', () => {
  vi.useFakeTimers();
  vi.setSystemTime(0);
  const store = new MaterialStore();
  store.put(release);
  expect(store.size).toBe(1);
  vi.advanceTimersByTime(10_000);
  expect(store.size).toBe(0);
});

it('delete_removes_immediately', () => {
  vi.useFakeTimers();
  vi.setSystemTime(0);
  const store = new MaterialStore();
  store.put(release);
  expect(store.delete('release')).toBe(true);
  expect(store.size).toBe(0);
  expect(vi.getTimerCount()).toBe(0);
});

it('credential revocation drops all material and fences late pushes', () => {
  const store = new MaterialStore(() => 1000);
  store.put(release);
  store.put({ ...release, release_id: 'other-release' });
  store.revokeCredential(release.credential_id);
  expect(store.size).toBe(0);
  expect(() => store.put(release)).toThrow();
  store.clear();
});

it('capability_for_other_step_cannot_use_material', async () => {
  const key = generateSigningKey();
  const verifier = new CapabilityVerifier({
    coreUrl: claims.iss, runnerIds: ['runner', 'second'], now: () => 1000,
    fetch: async () => Response.json({ keys: [key.jwk] }),
  });
  const verify = (overrides: object) => verifier.verifyCapability(signJwt(key, 'rb-grant+jwt', { ...claims, ...overrides }));
  const store = new MaterialStore(() => 1000);
  store.put(release);
  expect(store.forStep(await verify({}))).toEqual([release]);
  expect(store.forStep(await verify({ sub: 'different-step' }))).toEqual([]);
  expect(store.forStep(await verify({ generation: 2 }))).toEqual([]);
  await expect(verify({ aud: 'runner:second', runner_id: 'second' })).resolves.toMatchObject({ runner_id: 'second' });
  await expect(verify({ aud: 'runner:foreign', runner_id: 'foreign' })).rejects.toMatchObject({ status: 401 });
  await expect(verify({ exp: 1 })).rejects.toMatchObject({ status: 401 });
  for (const field of ['destructive', 'network', 'connections', 'model_profile', 'max_tokens', 'timeout_ms', 'step_deadline_ms']) {
    await expect(verify({ [field]: undefined })).rejects.toMatchObject({ status: 401 });
  }
  await expect(verify({ kind: 'credential' })).rejects.toMatchObject({ status: 401 });
  store.clear();
});

it('gateway_has_no_db_or_kms_config', () => {
  const token = 't'.repeat(32);
  const config = loadConfig('gateway', {
    RUNNER_ALLOWED_RUNNER_IDS: 'runner,second', RUNNER_ROLE: 'gateway', CORE_URL: 'https://core.example', API_URL: 'https://api.example',
    RUNNER_TOKEN_VAULT_GATEWAY: token, RUNNER_WORKER_GATEWAY_TOKENS: JSON.stringify({ runner: token + '1', second: token + '4' }),
    RUNNER_TOKEN_SANDBOXCTL_GATEWAY: token + '2', RUNNER_TOKEN_GATEWAY_API: token + '3',
  });
  expect(config.runnerIds).toEqual(['runner', 'second']);
  expect(config.databaseUrl).toBeUndefined();
  expect(config.schedulerDatabaseUrl).toBeUndefined();
  expect(config.kek).toBeUndefined();
});

it('ending_a_claim_fences_late_material_renewal_only_for_that_attempt', () => {
  const store = new MaterialStore(() => 1000);
  const attempt = claims as GrantClaims;
  store.put(release);
  store.endClaim(attempt);
  expect(store.forStep(attempt)).toEqual([]);
  expect(store.active(attempt)).toBe(false);
  expect(() => store.put(release)).toThrow('claim ended');
  store.put({ ...release, generation: 2 });
  expect(store.active({ ...attempt, generation: 2 })).toBe(true);
  store.clear();
});

it('preserves in-flight dispatch across an equivalent renewal until renewed expiry', () => {
  vi.useFakeTimers();
  vi.setSystemTime(0);
  const store = new MaterialStore();
  store.put(release);
  const signal = store.signal(release);
  const renewed = { ...structuredClone(release), expires_at: new Date(20_000).toISOString() };
  store.put(renewed);
  expect(store.signal(renewed)).toBe(signal);
  vi.advanceTimersByTime(10_000);
  expect(signal.aborted).toBe(false);
  vi.advanceTimersByTime(10_000);
  expect(signal.aborted).toBe(true);
});

it('aborts in-flight dispatch when a replacement changes the binding', () => {
  const store = new MaterialStore(() => 1000);
  store.put(release);
  const signal = store.signal(release);
  store.put({ ...release, binding: { origin: 'https://replacement.example' } });
  expect(signal.aborted).toBe(true);
  store.clear();
});

it('tracks OAuth use for the current generation and token version', () => {
  vi.useFakeTimers();
  vi.setSystemTime(1_000);
  const store = new MaterialStore();
  const oauth = { ...release, delivery: 'mcp' as const, material: {
    fields: {}, mcp_oauth: { access_token: 'fixture', generation: 1, token_version: 1 },
  } };
  store.put(oauth);
  expect(store.lastOAuthUse('credential', 1, 1)).toBeNull();
  store.noteOAuthUse(oauth);
  expect(store.lastOAuthUse('credential', 1, 1)).toBe(1_000);
  expect(store.lastOAuthUse('credential', 1, 2)).toBeNull();
  store.fenceOAuth('credential', 2);
  store.noteOAuthUse(oauth);
  expect(store.lastOAuthUse('credential', 1, 1)).toBeNull();
  store.clear();
});

it('accepts visible ASCII OAuth tokens and refuses placeholders as release material', async () => {
  const token = 't'.repeat(32);
  const store = new MaterialStore(() => 1000);
  const server = createGatewayServer({ inboundTokens: { vault: token }, log: createLogger('gateway-test') }, store);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const url = `http://127.0.0.1:${(server.address() as AddressInfo).port}/internal/material`;
  const push = (delivery: string, material: object) => fetch(url, { method: 'POST',
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ ...release, delivery, material }),
  });
  const mcp = (access_token: string) => push('mcp', { fields: {}, mcp_oauth: { access_token, generation: 1, token_version: 1 } });
  try {
    const notionToken = 'ntn_fixture:workspace-token';
    expect((await mcp(notionToken)).status).toBe(204);
    expect(store.get(release.release_id)?.material.mcp_oauth?.access_token).toBe(notionToken);
    for (const invalid of ['rb-placeholder-fixture', 'space token', 'control\n', 'unicode-é']) {
      expect((await mcp(invalid)).status).toBe(400);
    }
    for (const delivery of ['proxy', 'mcp', 'model']) {
      expect((await push(delivery, { fields: { key: 'rb-placeholder-fixture' } })).status).toBe(400);
    }
  } finally {
    store.clear();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});
