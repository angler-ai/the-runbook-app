#!/usr/bin/env node
// The image's one entry point: `runner <role>` or RUNNER_ROLE picks the
// process (api, worker, sandbox-ctl, vault, gateway), each deployed as its own
// container with its own identity (design §5.3). `runner migrate` applies the
// runner database migrations with the owner's URL (RUNNER_MIGRATE_DATABASE_URL).
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

const role = process.argv[2] ?? process.env.RUNNER_ROLE;
const target = role === undefined ? undefined : ROLES[role];
if (target === undefined) {
  console.error(`usage: runner <${Object.keys(ROLES).join('|')}> (or set RUNNER_ROLE)`);
  process.exit(2);
}
if (role !== 'migrate') process.env.RUNNER_ROLE ??= role;

try {
  const { main } = await import(target);
  await main();
} catch (error) {
  // Name and message only: no stack frames with argument values, no causes.
  const name = error instanceof Error ? error.name : 'Error';
  const message = error instanceof Error ? error.message : 'unknown failure';
  console.error(JSON.stringify({ level: 'fatal', process: role, error: name, message }));
  process.exit(1);
}
