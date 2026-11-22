import { describe, expect, it } from 'vitest';
import { ConfigError, loadConfig } from '../src/config.ts';

const token = (name: string) => `fake-${name}-`.padEnd(40, 'x');

function vaultEnv(): Record<string, string> {
  return {
    RUNNER_ROLE: 'vault',
    RUNNER_ALLOWED_RUNNER_IDS: 'runner',
    RUNNER_ORG_ID: 'org_test',
    CORE_URL: 'https://core.internal:8443',
    CORE_RELEASE_TOKEN: token('release'),
    RUNNER_DATABASE_URL: 'postgres://runner_vault:pw@db:5432/runner',
    GATEWAY_URL: 'https://gateway:8092',
    RUNNER_TOKEN_API_VAULT: token('api-vault'),
    RUNNER_TOKEN_WORKER_VAULT: token('worker-vault'),
    RUNNER_TOKEN_SANDBOXCTL_VAULT: token('sandboxctl-vault'),
    RUNNER_TOKEN_VAULT_GATEWAY: token('vault-gateway'),
    RUNNER_KEK_PROVIDER: 'local',
    RUNNER_KEK_LOCAL_FILE: '/run/secrets/kek',
  };
}

function problems(fn: () => unknown): string[] {
  try {
    fn();
  } catch (e) {
    if (e instanceof ConfigError) return e.problems;
    throw e;
  }
  throw new Error('expected a ConfigError');
}

describe('loadConfig', () => {
  it('config_lists_all_missing_vars', () => {
    const found = problems(() => loadConfig('vault', { RUNNER_ROLE: 'vault' }));
    for (const name of [
      'RUNNER_ORG_ID',
      'CORE_URL',
      'CORE_RELEASE_TOKEN',
      'RUNNER_DATABASE_URL',
      'GATEWAY_URL',
      'RUNNER_TOKEN_API_VAULT',
      'RUNNER_TOKEN_WORKER_VAULT',
      'RUNNER_TOKEN_VAULT_GATEWAY',
      'RUNNER_KEK_PROVIDER',
    ]) {
      expect(found.some((p) => p.includes(name)), `${name} in ${found.join('; ')}`).toBe(true);
    }
    // One error that names every problem, not the first one only.
    try {
      loadConfig('vault', { RUNNER_ROLE: 'vault' });
    } catch (e) {
      expect((e as Error).message).toContain('RUNNER_ORG_ID');
      expect((e as Error).message).toContain('RUNNER_KEK_PROVIDER');
    }
  });

  it('config_rejects_unknown_kek_provider', () => {
    const env = { ...vaultEnv(), RUNNER_KEK_PROVIDER: 'rot13' };
    expect(problems(() => loadConfig('vault', env)).join('\n')).toMatch(/RUNNER_KEK_PROVIDER.*rot13/);
    const later = { ...vaultEnv(), RUNNER_KEK_PROVIDER: 'aws-kms' };
    expect(problems(() => loadConfig('vault', later)).join('\n')).toMatch(/aws-kms.*not supported/);
  });

  it('loads a complete vault config', () => {
    const config = loadConfig('vault', vaultEnv());
    expect(config.role).toBe('vault');
    expect(config.kek).toEqual({ provider: 'local', file: '/run/secrets/kek' });
    expect(config.inboundTokens.api).toBe(token('api-vault'));
    expect(config.outboundTokens.gateway).toBe(token('vault-gateway'));
    expect(config.listen.port).toBe(8091);
  });

  it('gcp-kms needs its key name', () => {
    const env = { ...vaultEnv(), RUNNER_KEK_PROVIDER: 'gcp-kms' };
    delete (env as Record<string, string | undefined>).RUNNER_KEK_LOCAL_FILE;
    expect(problems(() => loadConfig('vault', env)).join('\n')).toContain('RUNNER_KEK_GCP_KEY');
  });

  it('refuses a role mismatch and short tokens', () => {
    const env = { ...vaultEnv(), RUNNER_ROLE: 'gateway', RUNNER_TOKEN_API_VAULT: 'short' };
    const found = problems(() => loadConfig('vault', env)).join('\n');
    expect(found).toContain('RUNNER_ROLE');
    expect(found).toMatch(/RUNNER_TOKEN_API_VAULT.*32/);
    expect(found).not.toContain('short');
  });

  it('gateway and sandbox-ctl refuse database and key settings', () => {
    const env = {
      RUNNER_ROLE: 'gateway',
      RUNNER_ALLOWED_RUNNER_IDS: 'runner',
      CORE_URL: 'https://core:8443',
      API_URL: 'https://api:8090',
      RUNNER_TOKEN_VAULT_GATEWAY: token('a'),
      RUNNER_TOKEN_WORKER_GATEWAY: token('b'),
      RUNNER_TOKEN_SANDBOXCTL_GATEWAY: token('c'),
      RUNNER_TOKEN_GATEWAY_API: token('d'),
    };
    expect(loadConfig('gateway', env).databaseUrl).toBeUndefined();
    const found = problems(() =>
      loadConfig('gateway', { ...env, RUNNER_DATABASE_URL: 'postgres://x', RUNNER_KEK_PROVIDER: 'local' }),
    ).join('\n');
    expect(found).toMatch(/RUNNER_DATABASE_URL must not be set/);
    expect(found).toMatch(/RUNNER_KEK_PROVIDER must not be set/);
  });

  it('never echoes token values in problems', () => {
    const env = { ...vaultEnv(), CORE_URL: 'not a url', RUNNER_TOKEN_WORKER_VAULT: 'tiny-secret' };
    const found = problems(() => loadConfig('vault', env)).join('\n');
    expect(found).not.toContain('tiny-secret');
  });

  it('requires https for CORE_URL unless RUNNER_INSECURE_CORE_HTTP=true', () => {
    const env = { ...vaultEnv(), CORE_URL: 'http://core.internal:8080' };
    expect(problems(() => loadConfig('vault', env)).join('\n')).toMatch(/CORE_URL must use https:/);
    // The internal-HTTP flag never relaxes the core's URL.
    expect(problems(() => loadConfig('vault', { ...env, RUNNER_INSECURE_INTERNAL_HTTP: 'true' })).join('\n')).toMatch(
      /CORE_URL must use https:/,
    );
    expect(loadConfig('vault', { ...env, RUNNER_INSECURE_CORE_HTTP: 'true' }).coreUrl).toBe('http://core.internal:8080');
  });

  it('requires https for internal URLs unless RUNNER_INSECURE_INTERNAL_HTTP=true', () => {
    const env = { ...vaultEnv(), GATEWAY_URL: 'http://gateway:8092' };
    expect(problems(() => loadConfig('vault', env)).join('\n')).toMatch(/GATEWAY_URL must use https:/);
    expect(problems(() => loadConfig('vault', { ...env, RUNNER_INSECURE_INTERNAL_HTTP: 'false' })).join('\n')).toMatch(
      /GATEWAY_URL must use https:/,
    );
    // The core flag doesn't relax internal URLs.
    expect(problems(() => loadConfig('vault', { ...env, RUNNER_INSECURE_CORE_HTTP: 'true' })).join('\n')).toMatch(
      /GATEWAY_URL must use https:/,
    );
    expect(loadConfig('vault', { ...env, RUNNER_INSECURE_INTERNAL_HTTP: 'true' }).urls.gateway).toBe('http://gateway:8092');
  });

  it('refuses insecure-http flags other than true or false', () => {
    const env = { ...vaultEnv(), RUNNER_INSECURE_INTERNAL_HTTP: 'yes', RUNNER_INSECURE_CORE_HTTP: '1' };
    const found = problems(() => loadConfig('vault', env)).join('\n');
    expect(found).toMatch(/RUNNER_INSECURE_INTERNAL_HTTP must be true or false/);
    expect(found).toMatch(/RUNNER_INSECURE_CORE_HTTP must be true or false/);
  });
});

it('caps the JWKS hard age at 24 hours during config load', () => {
  expect(() => loadConfig('vault', { ...vaultEnv(), RUNNER_JWKS_MAX_AGE_SECONDS: '86400' })).not.toThrow();
  expect(problems(() => loadConfig('vault', { ...vaultEnv(), RUNNER_JWKS_MAX_AGE_SECONDS: '86401' })))
    .toContain('RUNNER_JWKS_MAX_AGE_SECONDS must be an integer from 1 to 86400');
});
