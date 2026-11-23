import { createServer } from 'node:http';
import type { AddressInfo } from 'node:net';
import { canonicalJson } from '@runbook/runner-shared/binding';
import type { MaterialPush } from '@runbook/runner-shared/contracts/internal';
import { sealSecret } from '@runbook/runner-shared/seal-client';
import { hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { expect, it } from 'vitest';
import { credentialConfiguration } from '../src/credential-metadata.ts';
import { SecretManager, secretManagerResources } from '../src/secret-manager.ts';
import { DATADOG_CONFIG, ORG, TOKENS, startHarness } from './helpers.ts';

const reference = { type: 'gcp-secret-manager', resource: 'projects/test-project/secrets/upstream-key', version: 'latest' };

it('binds the Secret Manager resource and version for all supported credential types', () => {
  const configurations = {
    'api-key': DATADOG_CONFIG,
    'mcp-bearer': { url: 'https://mcp.example/tenant' },
    'model-provider': { provider: 'anthropic', endpoint: 'https://api.anthropic.com' },
  };
  for (const [type, config] of Object.entries(configurations)) {
    const bind = (source: unknown) => credentialConfiguration(type, { ...config, secret_source: source }, 'all');
    const { binding, hash } = bind(reference);
    expect(binding.secret_source).toEqual(reference);
    expect(hash.equals(bind({ ...reference, version: '1' }).hash)).toBe(false);
    expect(bind({ ...reference, version: 1 })).toEqual(bind({ ...reference, version: '1' }));
    expect(hash.equals(bind({ ...reference, resource: 'projects/test-project/secrets/replacement' }).hash)).toBe(false);
    expect(() => bind({ ...reference, resource: 'projects/test-project/secrets/../wrong' })).toThrow();
    expect(() => bind({ ...reference, version: 'current' })).toThrow();
  }
  expect(() => credentialConfiguration('mcp-oauth', {
    url: 'https://mcp.example/tenant', registration_mode: 'dynamic', secret_source: reference,
  }, 'all')).toThrow();
});

const policy = { resource: reference.resource, types: ['api-key'], destinations: DATADOG_CONFIG.hosts };

it('explains the JSON policy format when loading a legacy resource list', () => {
  for (const legacy of [reference.resource, JSON.stringify([reference.resource])]) {
    expect(() => secretManagerResources(legacy)).toThrow(/RUNNER_SECRET_MANAGER_RESOURCES.*JSON.*resource.*types.*destinations/);
  }
});

it.each([
  ['api-key', { hosts: ['api.datadoghq.com', 'alternate.example'] }, { hosts: ['api.datadoghq.com', 'outside.example'] }],
  ['mcp-bearer', { url: 'https://mcp.example/tenant' }, { url: 'https://mcp.example/tenant/other' }],
  ['model-provider', { endpoint: 'https://model.example/v1' }, { endpoint: 'https://model.example/v2' }],
])('checks the type, version and exact destinations for %s', (type, binding, outside) => {
  const reader = new SecretManager(secretManagerResources(JSON.stringify([{
    ...policy, types: [type], destinations: Object.values(binding).flat(), version: '2',
  }])));
  const pinned = { ...reference, type: 'gcp-secret-manager' as const, version: '2' };
  expect(() => reader.assertAllowed(pinned, type, binding)).not.toThrow();
  expect(() => reader.assertAllowed(pinned, type, outside)).toThrow(expect.objectContaining({ code: 'secret_manager.destination_not_allowed' }));
  expect(() => reader.assertAllowed({ ...pinned, version: 'latest' }, type, binding))
    .toThrow(expect.objectContaining({ code: 'secret_manager.not_allowed' }));
  expect(() => reader.assertAllowed(pinned, 'mcp-oauth', binding))
    .toThrow(expect.objectContaining({ code: 'secret_manager.not_allowed' }));
});

it.each([403, 404, 400, 429, 503, 'timeout'] as const)('classifies a Secret Manager failure (%s)', async (failure) => {
  const reader = new SecretManager(secretManagerResources(JSON.stringify([policy])),
    { getAccessToken: async () => 'fake-token' }, async () => {
      if (failure === 'timeout') throw new DOMException('timed out', 'TimeoutError');
      return Response.json({ error: { status: 'FAILED_PRECONDITION' } }, { status: failure });
    });
  await expect(reader.access({ ...reference, type: 'gcp-secret-manager' }, 'api-key', DATADOG_CONFIG))
    .rejects.toMatchObject({ code: [400, 403, 404].includes(Number(failure)) ?
      'secret_manager.unavailable' : 'secret_manager.retry' });
});

it('returns the concrete version for latest without losing precision', async () => {
  const version = '9223372036854775807';
  const reader = new SecretManager(secretManagerResources(JSON.stringify([policy])),
    { getAccessToken: async () => 'fake-token' }, async () => Response.json({
      name: `projects/123456789/secrets/upstream-key/versions/${version}`,
      payload: { data: Buffer.from('fixture').toString('base64') },
    }));
  await expect(reader.access({ ...reference, type: 'gcp-secret-manager' }, 'api-key', DATADOG_CONFIG))
    .resolves.toEqual({ version, value: Buffer.from('fixture') });
});

it('refuses unlisted resources before asking for the vault Google access token', async () => {
  const identity = { getAccessToken: async () => { throw new Error('unexpected identity request'); } };
  const reader = new SecretManager(secretManagerResources(JSON.stringify([policy])), identity);
  await expect(reader.access({ ...reference, type: 'gcp-secret-manager', resource: 'projects/test-project/secrets/vault-process' }, 'api-key', DATADOG_CONFIG))
    .rejects.toMatchObject({ code: 'secret_manager.not_allowed' });
});

it.skipIf(!hasTestDatabase)('resolves an approved Secret Manager reference at release and audits attempts', async () => {
  const first = 'fake-upstream-value-one';
  const second = 'fake-upstream-value-two';
  let value = first;
  let resolvedVersion = '1';
  const entries = secretManagerResources(JSON.stringify([policy]));
  let forbidden = false;
  const requests: { url?: string; authorization?: string }[] = [];
  const fake = createServer((request, response) => {
    requests.push({ url: request.url, authorization: request.headers.authorization });
    response.writeHead(forbidden ? 403 : 200, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ name: `projects/123456789/secrets/upstream-key/versions/${resolvedVersion}`, payload: { data: Buffer.from(value).toString('base64') } }));
  });
  await new Promise<void>((resolve) => fake.listen(0, '127.0.0.1', resolve));
  const pushes: MaterialPush[] = [];
  const h = await startHarness({
    secretManager: new SecretManager(entries, { getAccessToken: async () => 'fake-vault-google-token' }, (input, init) => {
      const target = new URL(String(input));
      expect(target.origin).toBe('https://secretmanager.googleapis.com');
      expect(init?.redirect).toBe('error');
      return fetch(`http://127.0.0.1:${(fake.address() as AddressInfo).port}${target.pathname}`, init);
    }),
    core: { baseUrl: 'https://core.example', releaseToken: TOKENS.release },
    gateway: { baseUrl: 'https://gateway.example', token: TOKENS.gateway, fetch: async (input, init) => {
      const path = new URL(String(input)).pathname;
      if (path === '/internal/material' && init?.method === 'POST') pushes.push(JSON.parse(String(init.body)) as MaterialPush);
      return path === '/internal/test' ? Response.json({ outcome: 'response', status: 200 }) : new Response(null, { status: 204 });
    } },
  });
  try {
    const created = await h.call('POST', '/internal/credential-metadata', {
      name: 'reference fixture', type: 'api-key', config: { ...DATADOG_CONFIG, secret_source: reference },
    });
    expect(created.status).toBe(200);
    expect(created.json.placeholder_eligible).toBe(true);
    const id = created.json.id as string;
    const key = await h.vault.ingestKey();
    async function confirm(plaintext: string, hash = created.json.binding_hash as string, expectedVersion = 0) {
      const blob = await sealSecret(key, { orgId: ORG, credentialId: id, field: 'key', expectedVersion,
        bindingHash: hash, value: plaintext, now: h.clock.now });
      return h.call('POST', '/internal/seal', { blob });
    }
    const open = () => h.vault.secrets.openForRelease({ credentialId: id });
    await expect(open()).rejects.toMatchObject({ code: 'secret.not_set' });
    expect((await confirm(first)).json.error.code).toBe('seal.reference_mismatch');
    expect((await confirm(canonicalJson(reference))).status).toBe(200);
    expect(requests).toEqual([]);

    expect((await h.call('POST', '/internal/test', { credential_id: id })).status).toBe(200);
    value = second;
    resolvedVersion = '2';
    expect((await h.call('POST', '/internal/test', { credential_id: id })).status).toBe(200);
    expect(pushes.map(({ material }) => material.fields.key)).toEqual([first, second]);
    expect(requests).toEqual(Array.from({ length: 2 }, () => ({
      url: `/v1/${reference.resource}/versions/latest:access`, authorization: 'Bearer fake-vault-google-token',
    })));

    entries.splice(0);
    await expect(open()).rejects.toMatchObject({ code: 'secret_manager.not_allowed' });
    expect((await h.call('POST', '/internal/test', { credential_id: id })).json.error.code).toBe('secret_manager.not_allowed');
    expect(pushes).toHaveLength(2);
    entries.push(...secretManagerResources(JSON.stringify([policy])));

    const redirected = await h.call('POST', `/internal/credential-metadata/${id}`, {
      config: { ...DATADOG_CONFIG, hosts: ['other.example'], secret_source: reference },
    });
    expect((await confirm(canonicalJson(reference), redirected.json.binding_hash, 1)).json.error.code)
      .toBe('secret_manager.destination_not_allowed');

    const pinned = { ...reference, version: '3' };
    resolvedVersion = '3';
    const changed = await h.call('POST', `/internal/credential-metadata/${id}`, {
      config: { ...DATADOG_CONFIG, secret_source: pinned },
    });
    expect(changed.json).toMatchObject({ status: 'needs_secret' });
    expect(changed.json.binding_hash).not.toBe(created.json.binding_hash);
    await expect(open()).rejects.toMatchObject({ code: 'vault.binding_mismatch' });
    expect(requests).toHaveLength(2);
    expect((await confirm(canonicalJson(reference), changed.json.binding_hash, 1)).status).toBe(400);
    expect((await confirm(canonicalJson(pinned), changed.json.binding_hash, 1)).status).toBe(200);
    value = 'rb-placeholder-fixture';
    await expect(open()).rejects.toMatchObject({ code: 'secret.invalid' });
    forbidden = true;
    await expect(open()).rejects.toMatchObject({ code: 'secret_manager.unavailable' });
    expect(requests.at(-1)?.url).toBe(`/v1/${reference.resource}/versions/3:access`);
    const { rows } = await h.db.admin().query(`SELECT detail FROM audit_log WHERE action = 'credential.secret_manager_access'
      AND resource_id = $1 ORDER BY seq`, [id]);
    expect(rows.slice(0, 2).map(({ detail }) => detail.secret_manager_version)).toEqual(['1', '2']);
    expect(rows.map(({ detail }) => detail.outcome)).toEqual(['accessed', 'accessed', 'failed', 'failed', 'failed', 'failed']);
    const versions = await h.db.admin().query('SELECT * FROM credential_secret_versions WHERE credential_id = $1', [id]);
    expect(versions.rowCount).toBe(2);
    for (const secret of [first, second]) {
      expect(JSON.stringify(versions.rows)).not.toContain(secret);
      expect(JSON.stringify(rows)).not.toContain(secret);
      expect(h.logs.text()).not.toContain(secret);
    }
  } finally {
    await h.close();
    fake.closeAllConnections();
    await new Promise<void>((resolve) => fake.close(() => resolve()));
  }
});
