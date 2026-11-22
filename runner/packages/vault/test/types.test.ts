// The v1 credential types (design §7.2): their destination bindings, and
// that the binding (including allowed groups) is what the secret is sealed
// to, so changing it in config stops the secret from opening.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { bindingHash } from '@runbook/runner-shared/binding';
import { CREDENTIAL_TYPES } from '@runbook/runner-shared/credential-types';
import { FakeCore } from '@runbook/runner-shared/test-utils/fake-core';
import { FakeGateway } from '@runbook/runner-shared/test-utils/fake-gateway';
import { hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { sealSecret } from '@runbook/runner-shared/seal-client';
import vectors from '../../../../api/runner-binding-vectors.json' with { type: 'json' };
import { sharedWith } from '../src/policy.ts';
import { credentialBinding, typeImpl } from '../src/types/index.ts';
import { DATADOG_CONFIG, type Harness, insertCredential, ORG, startHarness, TOKENS, updateCredentialConfig } from './helpers.ts';

const hash = (type: string, config: Record<string, unknown>, groups: 'all' | string[] = 'all') =>
  bindingHash(type, credentialBinding(type, config, groups)).toString('hex');

describe('credential types', () => {
  it('match their shared manifests', () => {
    for (const manifest of Object.values(CREDENTIAL_TYPES)) {
      const impl = typeImpl(manifest.type);
      expect(impl.secretFields).toEqual(manifest.secretFields.map((f) => f.name));
      expect(impl.deliveries).toEqual(manifest.deliveries);
    }
  });

  it.each(vectors)('matches the client binding vectors for $type', (fixture) => {
    vi.stubEnv('RUNNER_ALLOW_HTTP_MODEL_ENDPOINTS', 'true');
    try {
      const binding = credentialBinding(fixture.type, fixture.config, fixture.groups as 'all' | string[]);
      expect(binding).toEqual(fixture.binding);
      expect(bindingHash(fixture.type, binding).toString('base64url')).toEqual(fixture.hash);
    } finally {
      vi.unstubAllEnvs();
    }
  });

  it('api_key_binding_includes_hosts_and_header_format', () => {
    const binding = credentialBinding('api-key', DATADOG_CONFIG, 'all');
    expect(binding).toEqual({
      hosts: ['api.datadoghq.com'],
      header_name: 'dd-api-key',
      header_format: '{}',
      test: { method: 'GET', path: '/api/v1/validate' },
      groups: 'all',
    });
    const base = hash('api-key', DATADOG_CONFIG);
    expect(hash('api-key', { ...DATADOG_CONFIG, hosts: ['api.datadoghq.eu'] })).not.toBe(base);
    expect(hash('api-key', { ...DATADOG_CONFIG, hosts: ['api.datadoghq.com', 'evil.example'] })).not.toBe(base);
    expect(hash('api-key', { ...DATADOG_CONFIG, header_name: 'Authorization' })).not.toBe(base);
    expect(hash('api-key', { ...DATADOG_CONFIG, header_format: 'Bearer {}' })).not.toBe(base);
    // Normalization: case, order and duplicates don't change the destination…
    expect(hash('api-key', { ...DATADOG_CONFIG, hosts: ['API.DatadogHQ.com', 'api.datadoghq.com'], header_name: 'dd-api-key' })).toBe(base);
    // …but the test request is: it is where the key goes when someone clicks Test.
    expect(hash('api-key', { ...DATADOG_CONFIG, test: { method: 'GET', path: '/other' } })).not.toBe(base);
    expect(hash('api-key', { ...DATADOG_CONFIG, test: { method: 'HEAD', path: '/api/v1/validate' } })).not.toBe(base);
    const { test: _test, ...noTest } = DATADOG_CONFIG;
    expect(credentialBinding('api-key', noTest, 'all').test).toEqual({ method: 'GET', path: '/' });
    for (const test of [{ path: 'relative' }, { path: '//evil.example/x' }, { path: '/a b' }, { path: '/x#frag' }, { method: 'POST', path: '/' }]) {
      expect(() => credentialBinding('api-key', { ...DATADOG_CONFIG, test }, 'all'), JSON.stringify(test)).toThrow(/test/);
    }
    // The test request is built from the binding only, and refused if the config says otherwise.
    expect(typeImpl('api-key').testRequest(binding, DATADOG_CONFIG)).toEqual({
      method: 'GET',
      url: 'https://api.datadoghq.com/api/v1/validate',
    });
    expect(() => typeImpl('api-key').testRequest(binding, { ...DATADOG_CONFIG, test: { method: 'GET', path: '/other' } })).toThrow();
    // Malformed destinations are refused, naming the field only.
    expect(() => credentialBinding('api-key', { ...DATADOG_CONFIG, hosts: ['https://x.example/path'] }, 'all')).toThrow(/hosts/);
    expect(() => credentialBinding('api-key', { ...DATADOG_CONFIG, header_format: 'Bearer' }, 'all')).toThrow(/header_format/);
    expect(() => credentialBinding('api-key', { ...DATADOG_CONFIG, header_format: 'x {}\r\nInjected: {}' }, 'all')).toThrow();
  });

  it('model_provider_binding_is_endpoint', () => {
    const config = { provider: 'anthropic', endpoint: 'https://api.anthropic.com/', models: ['claude-sonnet-5-5'] };
    expect(credentialBinding('model-provider', config, 'all')).toEqual({
      provider: 'anthropic',
      endpoint: 'https://api.anthropic.com',
      header_name: 'x-api-key',
      header_format: '{}',
      groups: 'all',
    });
    const base = hash('model-provider', config);
    expect(hash('model-provider', { ...config, endpoint: 'https://llm-proxy.example' })).not.toBe(base);
    expect(hash('model-provider', { ...config, provider: 'openai-compatible' })).not.toBe(base);
    // Model lists and prices aren't destinations.
    expect(hash('model-provider', { ...config, models: [] })).toBe(base);
    expect(credentialBinding('model-provider', { provider: 'openai', endpoint: 'https://api.openai.com/v1' }, 'all')).toMatchObject({
      header_name: 'authorization',
      header_format: 'Bearer {}',
    });
    expect(() => credentialBinding('model-provider', { provider: 'anthropic', endpoint: 'http://api.anthropic.com' }, 'all')).toThrow(/https/);
    expect(() => credentialBinding('model-provider', { provider: 'mystery', endpoint: 'https://x.example' }, 'all')).toThrow(/provider/);
  });

  it('mcp_bearer_binding_is_the_server_url', () => {
    const binding = credentialBinding('mcp-bearer', { url: 'https://MCP.example.com/mcp/' }, ['rg_b', 'rg_a', 'rg_a']);
    expect(binding).toEqual({
      url: 'https://mcp.example.com/mcp',
      header_name: 'authorization',
      header_format: 'Bearer {}',
      groups: ['rg_a', 'rg_b'],
    });
    expect(typeImpl('mcp-bearer').deliveries).toEqual(['mcp']);
    expect(typeImpl('mcp-bearer').testRequest(binding, {})).toMatchObject({ method: 'POST', url: 'https://mcp.example.com/mcp' });
  });

  it('groups are part of every binding', () => {
    for (const [type, config] of [
      ['api-key', DATADOG_CONFIG],
      ['model-provider', { provider: 'anthropic', endpoint: 'https://api.anthropic.com' }],
      ['mcp-bearer', { url: 'https://mcp.example.com/mcp' }],
    ] as const) {
      expect(hash(type, config, ['rg_prod_write'])).not.toBe(hash(type, config, 'all'));
      expect(hash(type, config, ['rg_prod_write'])).not.toBe(hash(type, config, ['rg_readonly']));
    }
  });

  it('sharedWith: none by default, all, or a list', () => {
    expect(sharedWith({ projects: 'none' }, 'pr_ops')).toBe(false);
    expect(sharedWith({}, 'pr_ops')).toBe(false);
    expect(sharedWith(null, 'pr_ops')).toBe(false);
    expect(sharedWith({ projects: 'all' }, 'pr_ops')).toBe(true);
    expect(sharedWith({ projects: ['pr_ops'] }, 'pr_ops')).toBe(true);
    expect(sharedWith({ projects: ['pr_data'] }, 'pr_ops')).toBe(false);
  });
});

describe.skipIf(!hasTestDatabase)('credential types in the database', () => {
  let h: Harness;
  let core: FakeCore;
  let gateway: FakeGateway;
  const SECRET = 'fake-prod-write-key-EEEEEEEEEEEEEEEEEEEE';

  beforeEach(async () => {
    const clock = { now: Date.now() };
    core = await new FakeCore({ now: () => clock.now, releaseToken: TOKENS.release }).start();
    gateway = await new FakeGateway(TOKENS.gateway).start();
    h = await startHarness({
      clock,
      core: { baseUrl: core.url, releaseToken: TOKENS.release },
      gateway: { baseUrl: gateway.url, token: TOKENS.gateway },
    });
  });
  afterEach(async () => {
    await h?.close();
    await core?.close();
    await gateway?.close();
  });

  it('unlabeled_credential_defaults_destructive and sharing_defaults_to_none (schema defaults)', async () => {
    await h.db.admin().query(`INSERT INTO credentials (id, org_id, name, type) VALUES ('cr_bare', $1, 'bare', 'api-key')`, [ORG]);
    const { rows } = await h.db.admin().query(`SELECT label, sharing, groups, status FROM credentials WHERE id = 'cr_bare'`);
    expect(rows[0]).toEqual({ label: 'destructive', sharing: { projects: 'none' }, groups: 'all', status: 'needs_secret' });
  });

  it('binding_includes_allowed_groups', async () => {
    const credential = await insertCredential(h, { name: 'prod-write-key', groups: ['rg_prod_write'] });
    const key = (await h.call('GET', '/internal/ingest-key')).json;
    const blob = await sealSecret(key, {
      orgId: ORG,
      credentialId: credential.id,
      field: 'key',
      expectedVersion: 0,
      bindingHash: credential.hash,
      value: SECRET,
      now: h.clock.now,
    });
    expect((await h.call('POST', '/internal/seal', { blob })).status).toBe(200);
    // Two agents: one in prod-write's group, one in the default group, both listing the credential.
    for (const [agent, group] of [
      ['ag_writer', 'rg_prod_write'],
      ['ag_reader', 'rg_default'],
    ]) {
      await h.db.admin().query(
        `INSERT INTO agents (id, project_id, group_id, name, status, current_version_id) VALUES ($1, 'pr_ops', $2, $1, 'active', $3)`,
        [agent, group, `${agent}_v1`],
      );
      await h.db.admin().query(
        `INSERT INTO agent_versions (id, agent_id, version, config, ceiling, created_by) VALUES ($1, $2, 1, $3, '{}', 'us_admin')`,
        [`${agent}_v1`, agent, JSON.stringify({ credentials: [{ credential_id: credential.id, delivery: 'proxy' }] })],
      );
    }
    const releaseAs = async (sr: string, agent: string, runner: string, group: string) => {
      const lease = core.lease(sr, { agent_id: agent, agent_version: `${agent}_v1`, runner_id: runner, group_id: group, credentials: [credential.id] });
      const cap = await h.call('POST', '/internal/capability', { step_run_id: sr, claim_id: lease.claim_id, generation: 1, runner_id: runner }, runner === 'rn_prod' ? TOKENS.workerProd : TOKENS.worker);
      expect(cap.status).toBe(200);
      return h.call('POST', '/internal/release', { capability: cap.json.capability, credential_id: credential.id, delivery: 'proxy' }, runner === 'rn_prod' ? TOKENS.workerProd : TOKENS.worker);
    };
    // Released to a step in prod-write…
    expect((await releaseAs('sr_w', 'ag_writer', 'rn_prod', 'rg_prod_write')).status).toBe(200);
    // …refused by policy for a step on the default group's runner…
    const refused = await releaseAs('sr_r', 'ag_reader', 'rn_one', 'rg_default');
    expect(refused.json.error.code).toBe('release.not_allowed');
    // …and when a compromised runner-api widens the groups, the policy passes
    // but the secret no longer opens: the groups are in the sealed binding.
    await updateCredentialConfig(h, credential.id, DATADOG_CONFIG, ['rg_prod_write', 'rg_default']);
    const widened = await releaseAs('sr_r2', 'ag_reader', 'rn_one', 'rg_default');
    expect(widened.status).toBe(409);
    expect(widened.json.error.code).toBe('vault.binding_mismatch');
    expect(gateway.pushes.map((p) => p.step_run_id)).toEqual(['sr_w']);
    expect(core.revoked.at(-1)).toMatchObject({ step_run_id: 'sr_r2', reason: 'vault.binding_mismatch' });
    expect(h.logs.text()).toContain('binding mismatch at unwrap');
    expect(h.logs.text()).not.toContain(SECRET);
  });

  it('changing the api-key test path requires entering the secret again', async () => {
    const credential = await insertCredential(h, { name: 'dd-test-path' });
    const key = (await h.call('GET', '/internal/ingest-key')).json;
    const blob = await sealSecret(key, {
      orgId: ORG,
      credentialId: credential.id,
      field: 'key',
      expectedVersion: 0,
      bindingHash: credential.hash,
      value: SECRET,
      now: h.clock.now,
    });
    expect((await h.call('POST', '/internal/seal', { blob })).status).toBe(200);
    expect((await h.call('POST', '/internal/test', { credential_id: credential.id })).status).toBe(200);
    const pushes = gateway.pushes.length;
    // A compromised runner-api points the test somewhere else on the host.
    await updateCredentialConfig(h, credential.id, { ...DATADOG_CONFIG, test: { method: 'GET', path: '/api/v1/echo-headers' } });
    const res = await h.call('POST', '/internal/test', { credential_id: credential.id });
    expect(res.status).toBe(409);
    expect(res.json.error.code).toBe('vault.binding_mismatch');
    expect(gateway.pushes).toHaveLength(pushes);
  });
});
