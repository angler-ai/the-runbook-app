import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { expect, it } from 'vitest';

const entry = new URL('../../../bin/runner.mjs', import.meta.url);
const log = new URL('../../shared/src/log.ts', import.meta.url).href;
const key = Buffer.alloc(32, 7).toString('base64url');

// Exercise the real entry point and identity command with a fixture identity
// store so the process behavior is covered without a database or KEK.
const fixture = `
  import { registerHooks } from 'node:module';
  const modules = {
    './identity.ts': \`
      import { createLogger } from ${JSON.stringify(log)};
      export const VaultIdentity = { async load() {
        createLogger('vault').info('fixture identity store opened');
        if (process.env.FIXTURE_STARTUP_FAILURE) {
          setInterval(() => {}, 60_000);
          throw new Error('fixture startup failure');
        }
        return { publicKey: '${key}' };
      } };
    \`,
    './kek/index.ts': 'export async function createKek() {}',
    '@runbook/runner-shared/db': 'export function openPool() { return { async end() {} }; }',
  };
  registerHooks({ resolve(specifier, context, next) {
    if (context.parentURL?.endsWith('/vault/src/identity-main.ts') && Object.hasOwn(modules, specifier)) {
      return { url: 'data:text/javascript,' + encodeURIComponent(modules[specifier]), shortCircuit: true };
    }
    return next(specifier, context);
  } });
`;

function command(fail: boolean) {
  return spawnSync(process.execPath, [
    '--conditions=development', '--import', 'data:text/javascript,' + encodeURIComponent(fixture),
    fileURLToPath(entry), 'vault-identity',
  ], {
    env: { PATH: process.env.PATH, RUNNER_KEK_PROVIDER: 'local', RUNNER_KEK_LOCAL_FILE: '/fixture',
      RUNNER_DATABASE_URL: 'fixture', ...(fail && { FIXTURE_STARTUP_FAILURE: '1' }) },
    encoding: 'utf8',
    timeout: 4000,
  });
}

it('prints only the key on stdout for vault-identity', () => {
  const result = command(false);
  expect(result.status).toBe(0);
  expect(result.stdout).toBe(key + '\n');
  expect(result.stderr).toContain('fixture identity store opened');
});

it('flushes the error and exits 1 even when startup leaves an open handle', () => {
  const result = command(true);
  expect(result.status).toBe(1);
  expect(result.stderr).toContain('runner startup failed');
  expect(result.stdout).toBe('');
});
