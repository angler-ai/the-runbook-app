import { loadConfig, required } from '@runbook/runner-shared/config';
import type { SettleRequest } from '@runbook/runner-shared/contracts/internal';
import { openPool } from '@runbook/runner-shared/db';
import { reconcile } from './settle.ts';

const USAGE = 'Usage: runner ledger-reconcile list | apply [--override-hold] (JSON on stdin)';
const ID = /^[A-Za-z0-9_-]{1,128}$/;

export async function main(): Promise<void> {
  const [operation, ...extra] = process.argv.slice(3);
  const overrideHold = operation === 'apply' && extra.length === 1 && extra[0] === '--override-hold';
  if ((operation !== 'list' && operation !== 'apply') || (extra.length && !overrideHold)) throw new Error(USAGE);
  const config = loadConfig('api');
  const pool = openPool('scheduler', required(config.schedulerDatabaseUrl, 'RUNNER_SCHEDULER_DATABASE_URL'));
  try {
    if (operation === 'list') {
      const { rows } = await pool.query(
        `SELECT r.id AS reservation_id, r.step_run_id, r.runner_id, r.amount_usd, r.actual_usd, r.settled_at,
           (u.id IS NOT NULL) AS usage_event_linked
         FROM ledger_reservations r LEFT JOIN usage_events u ON u.reservation_id = r.id
         WHERE r.kind = 'model' AND r.usage_pending ORDER BY r.settled_at, r.id LIMIT 1000`);
      process.stdout.write(`${JSON.stringify(rows, null, 2)}\n`);
      return;
    }
    process.stdin.setEncoding('utf8');
    let source = '';
    for await (const chunk of process.stdin) {
      source += chunk;
      if (source.length > 8192) throw new Error('reconciliation input is too large');
    }
    let request: (SettleRequest & { step_run_id: string; runner_id: string }) | undefined;
    try { request = JSON.parse(source); } catch { throw new Error('invalid reconciliation JSON'); }
    if (!request || ![request.reservation_id, request.step_run_id, request.runner_id].every((id) => typeof id === 'string' && ID.test(id)) ||
        typeof request.actual_usd !== 'number' || !request.usage || typeof request.usage !== 'object' ||
        Array.isArray(request.usage) || Object.keys(request.usage).some((key) =>
          !['input_tokens', 'output_tokens', 'cached_tokens', 'cache_write_tokens'].includes(key))) {
      throw new Error('expected reservation_id, step_run_id, runner_id, actual_usd and measured usage counters');
    }
    await reconcile(pool, request, { stepId: request.step_run_id, runnerId: request.runner_id, overrideHold });
    process.stdout.write('Model usage reconciled.\n');
  } finally {
    await pool.end();
  }
}
