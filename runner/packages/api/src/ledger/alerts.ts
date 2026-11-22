import { randomUUID } from 'node:crypto';
import type pg from 'pg';
import { withTransaction } from '@runbook/runner-shared/db';
import { micros, type BucketRow } from './buckets.ts';

export interface BudgetAlert {
  level: string;
  period: string;
  key: string;
  threshold: 50 | 80 | 100;
}

export function crossedThresholds(bucket: BucketRow, actual: number): BudgetAlert[] {
  if (bucket.cap_usd === null) return [];
  const cap = micros(bucket.cap_usd);
  const spent = micros(bucket.spent_usd) + actual;
  return ([50, 80, 100] as const)
    .filter((threshold) => !bucket.alerts_sent.includes(threshold) && cap > 0 && spent * 100 >= cap * threshold)
    .map((threshold) => ({ level: bucket.level, period: bucket.period, key: bucket.key, threshold }));
}


export async function enqueueBudgetAlerts(client: pg.PoolClient, alerts: BudgetAlert[], groupId: string, runnerId: string): Promise<void> {
  for (const alert of alerts) {
    const id = randomUUID();
    await client.query('INSERT INTO budget_event_outbox(id, group_id, runner_id, data) VALUES ($1,$2,$3,$4)',
      [id, groupId, runnerId, JSON.stringify({ ...alert, event_id: id, group_id: groupId, runner_id: runnerId })]);
  }
}

export async function deliverBudgetAlerts(pool: pg.Pool, options: {
  coreUrl: string; installId: string; token: string; fetch?: typeof fetch;
}, now = new Date()): Promise<void> {
  for (let i = 0; i < 25; i++) {
    const found = await withTransaction(pool, async (client) => {
      const { rows: [event] } = await client.query<{ id: string; data: unknown; attempts: number; kind: string }>(
        `SELECT id, data, attempts, kind FROM budget_event_outbox WHERE next_attempt_at <= $1
         ORDER BY next_attempt_at, id LIMIT 1 FOR UPDATE SKIP LOCKED`, [now],
      );
      if (!event) return false;
      let delivered = false;
      try {
        const response = await (options.fetch ?? fetch)(
          `${options.coreUrl}/v1/runner-installs/${encodeURIComponent(options.installId)}/events`, {
            method: 'POST', headers: { authorization: `Bearer ${options.token}`, 'content-type': 'application/json' },
            redirect: 'error', signal: AbortSignal.timeout(10_000),
            body: JSON.stringify({ kind: event.kind, data: event.data }),
          });
        delivered = response.ok;
      } catch {
        // A later pass can deliver the durable event.
      }
      if (delivered) await client.query('DELETE FROM budget_event_outbox WHERE id = $1', [event.id]);
      else {
        const retryAt = new Date(now.getTime() + Math.min(3_600_000, 30_000 * 2 ** Math.min(7, event.attempts)));
        await client.query('UPDATE budget_event_outbox SET attempts = attempts + 1, next_attempt_at = $2 WHERE id = $1',
          [event.id, retryAt]);
      }
      return true;
    });
    if (!found) return;
  }
}
