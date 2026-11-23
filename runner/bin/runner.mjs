#!/usr/bin/env node
// The image's one entry point: `runner <role>` or RUNNER_ROLE picks the
// process (api, worker, sandbox-ctl, vault, gateway), each deployed as its own
// container with its own identity (design §5.3). `runner migrate` applies the
// runner database migrations with the owner's URL (RUNNER_MIGRATE_DATABASE_URL).
import { readFile } from 'node:fs/promises';

const ROLES = {
  api: '@runbook/runner-api/main',
  worker: '@runbook/runner-worker/main',
  'sandbox-ctl': '@runbook/runner-sandbox-ctl/main',
  vault: '@runbook/runner-vault/main',
  'ledger-reconcile': '@runbook/runner-api/ledger/reconcile-main',
  'vault-identity': '@runbook/runner-vault/identity-main',
  gateway: '@runbook/runner-gateway/main',
  migrate: '@runbook/runner-shared/migrate-main',
};

import { createLogger, errorFields, initLogRedaction, setLogSink } from '@runbook/runner-shared/log';
import { initTelemetry } from '@runbook/runner-shared/telemetry';

const role = process.argv[2] ?? process.env.RUNNER_ROLE;
const target = role === undefined ? undefined : ROLES[role];
if (target === undefined) {
  console.error(`usage: runner <${Object.keys(ROLES).join('|')}> (or set RUNNER_ROLE)`);
  process.exit(2);
}
if (role !== 'migrate') process.env.RUNNER_ROLE ??= role;
const logStream = ['migrate', 'ledger-reconcile', 'vault-identity'].includes(role) ? process.stderr : process.stdout;
setLogSink((line) => logStream.write(line + '\n'));

let provider;
try {
  if (process.env.RUNNER_SECRET_ENV_FILE) {
    let secrets;
    try {
      secrets = JSON.parse(await readFile(process.env.RUNNER_SECRET_ENV_FILE, 'utf8'));
      if (!secrets || Array.isArray(secrets) || typeof secrets !== 'object') throw new Error();
      for (const [name, value] of Object.entries(secrets)) {
        const vaultResourceList = role === 'vault' && name === 'RUNNER_SECRET_MANAGER_RESOURCES';
        if ((!vaultResourceList && !/^(RUNNER_(TOKEN_[A-Z_]+|WORKER_(API_TOKENS|VAULT_TOKENS|GATEWAY_TOKENS|NOTIFIERS)|METRICS_TOKEN|CORE_WEBHOOK_SECRET)|CORE_(CLAIM|DIRECTORY|RELEASE)_TOKEN)$/.test(name)) ||
            typeof value !== 'string' || Object.hasOwn(process.env, name)) throw new Error();
        process.env[name] = value;
      }
    } catch {
      throw new Error('invalid runner process secret mount');
    }
  }
  await initLogRedaction();
  provider = initTelemetry(role);
  process.once('beforeExit', () => { void provider.shutdown().catch(() => undefined); });
  const { main } = await import(target);
  await main();
} catch (error) {
  createLogger(role).error('runner startup failed', errorFields(error));
  let timeout;
  try {
    await Promise.race([
      Promise.allSettled([provider?.shutdown(), new Promise((resolve) => logStream.write('', resolve))]),
      new Promise((resolve) => { timeout = setTimeout(resolve, 1000); }),
    ]);
  } finally {
    clearTimeout(timeout);
    process.exit(1);
  }
}
