import { describe, expect, it } from 'vitest';
import { CREDENTIAL_TYPES, DEFAULT_SHARING, effectiveLabel, validateCredentialConfig } from '../src/credential-types.ts';
import { findSecretShapedValues } from '../src/secret-shape.ts';

// Obviously fake values in real token shapes.
const FAKE_ANTHROPIC = 'sk-ant-api03-FAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKE';
const FAKE_GITHUB = 'ghp_FAKEfakeFAKEfakeFAKEfakeFAKEfake1234';
// 40 chars, fake. Joined at runtime so secret scanners don't read the literal as a cloud key.
const HIGH_ENTROPY = ['q7Zr2LxW9vTn4KpB', '8sYd3HfM6cJg1QaE5uRo0NiV'].join('');

describe('secret-shaped config values', () => {
  it('config_rejects_secret_shaped_values', () => {
    const base = { hosts: ['api.example.com'], header_name: 'Authorization', header_format: 'Bearer {}' };
    const cases: [string, Record<string, unknown>][] = [
      ['anthropic key', { ...base, header_format: `Bearer ${FAKE_ANTHROPIC}` }],
      ['github token', { ...base, hosts: [FAKE_GITHUB] }],
      ['high entropy', { ...base, test: { path: `/check/${HIGH_ENTROPY}` } }],
      ['url credentials', { ...base, hosts: ['https://user:pass@host.example'] }],
    ];
    for (const [name, config] of cases) {
      const problems = validateCredentialConfig('api-key', config);
      expect(problems.length, name).toBeGreaterThan(0);
      expect(problems.join('\n'), name).toMatch(/put this in a secret field/);
      // Problems name the field, never the value.
      for (const v of [FAKE_ANTHROPIC, FAKE_GITHUB, HIGH_ENTROPY, 'user:pass']) {
        expect(problems.join('\n')).not.toContain(v);
      }
    }
    expect(validateCredentialConfig('model-provider', { provider: 'openai-compatible', endpoint: 'https://u:p@llm.example/v1' }).join()).toMatch(
      /secret field/,
    );
  });

  it('finds each shape with its path', () => {
    expect(findSecretShapedValues({ a: { b: [FAKE_ANTHROPIC] } })).toEqual([{ path: 'a.b[0]', reason: 'known-prefix' }]);
    expect(findSecretShapedValues({ k: HIGH_ENTROPY })).toEqual([{ path: 'k', reason: 'high-entropy' }]);
    expect(findSecretShapedValues({ u: 'postgres://app:hunter2@db:5432/x' })).toEqual([{ path: 'u', reason: 'url-credentials' }]);
    expect(findSecretShapedValues({ p: '-----BEGIN PRIVATE KEY-----\nAAAA' })).toEqual([{ path: 'p', reason: 'private-key' }]);
    for (const shape of ['xoxb-FAKE-1234-fake', 'AKIAFAKEFAKEFAKEFAKE', 'github_pat_FAKE_fake_FAKE_fake12']) {
      expect(findSecretShapedValues({ v: shape }), shape).toHaveLength(1);
    }
  });

  it('leaves ordinary config alone', () => {
    const ordinary = {
      hosts: ['api.datadoghq.com', 'api.us5.datadoghq.com'],
      header_name: 'DD-API-KEY',
      header_format: 'Bearer {}',
      endpoint: 'https://api.anthropic.com',
      url: 'https://mcp.example.com/mcp',
      service_account: 'sa-viewer@angler-prd.iam.gserviceaccount.com',
      id: '123e4567-e89b-12d3-a456-426614174000',
      commit: '9fceb02d0ae598e95dc970b74767f19372d61af8',
      path: '/api/v1/validate',
      region: 'us-central1',
      model: 'claude-sonnet-5-5-20260101',
    };
    expect(findSecretShapedValues(ordinary)).toEqual([]);
    expect(validateCredentialConfig('api-key', ordinary)).toEqual([]);
  });

  it('checks required fields', () => {
    expect(validateCredentialConfig('api-key', {}).join('\n')).toMatch(/hosts is required/);
    expect(validateCredentialConfig('mcp-bearer', { url: 'https://mcp.example.com/mcp' })).toEqual([]);
    expect(validateCredentialConfig('nope' as never, {}).join()).toMatch(/unknown credential type/);
  });

  it('unlabeled_credential_defaults_destructive', () => {
    expect(CREDENTIAL_TYPES['api-key'].defaultLabel).toBe('destructive');
    expect(CREDENTIAL_TYPES['mcp-bearer'].defaultLabel).toBe('destructive');
    expect(CREDENTIAL_TYPES['model-provider'].defaultLabel).toBe('read');
    expect(effectiveLabel(undefined)).toBe('destructive');
    expect(effectiveLabel(null)).toBe('destructive');
    expect(effectiveLabel('read')).toBe('read');
  });

  it('sharing_defaults_to_none', () => {
    expect(DEFAULT_SHARING).toEqual({ projects: 'none' });
  });
});
