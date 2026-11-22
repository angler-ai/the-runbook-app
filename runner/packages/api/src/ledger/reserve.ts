import { randomUUID } from 'node:crypto';
import type pg from 'pg';
import { withTransaction } from '@runbook/runner-shared/db';
import { budgetBuckets, dollars, lockBuckets, micros, type BudgetContext } from './buckets.ts';

export class CapExceeded extends Error {
  readonly level: string;
  constructor(level: string) {
    super('budget reached');
    this.level = level;
  }
}

export async function reserve(pool: pg.Pool, context: BudgetContext, amount: number,
  kind: 'model' | 'compute' | 'admission', now = new Date()): Promise<{ reservation_id: string } | { level: string }> {
  try {
    return await withTransaction(pool, (client) => reserveInTransaction(client, context, amount, kind, now));
  } catch (error) {
    if (error instanceof CapExceeded) return { level: error.level };
    throw error;
  }
}

export async function reserveInTransaction(client: pg.PoolClient, context: BudgetContext, amount: number,
  kind: 'model' | 'compute' | 'admission', now: Date, minimum?: number, headroom = 0): Promise<{ reservation_id: string; amount_usd?: number }> {
  let requested = micros(amount, 'ceil');
  const floor = minimum === undefined ? requested : micros(minimum, 'ceil');
  const protectedRoom = micros(headroom, 'ceil');
  if (floor > requested) throw new Error('minimum exceeds reservation');
  const admission = kind === 'model' ? (await client.query<{ id: string; bucket_ids: string[]; amount_usd: string }>(
    `SELECT id, bucket_ids, amount_usd FROM ledger_reservations
     WHERE step_run_id = $1 AND runner_id = $2 AND kind = 'admission' AND status = 'reserved'
     ORDER BY id FOR UPDATE`, [context.stepId, context.runnerId],
  )).rows : [];
  const buckets = budgetBuckets(context, now);
  const sorted = [...buckets].sort((a, b) => a.id.localeCompare(b.id, 'en'));
  for (const row of sorted) {
    // The deterministic ID and the natural key both identify the bucket.
    // A concurrent insert can hit either unique index.
    await client.query(
      `INSERT INTO ledger_buckets(id, level, key, period, period_start, group_id, runner_id, cap_usd)
       VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING`,
      [row.id, row.level, row.key, row.period, row.start,
        ['org', 'project', 'agent', 'run'].includes(row.level) ? null : context.groupId,
        row.level === 'step' ? context.runnerId : null,
        row.cap === null ? null : dollars(micros(row.cap, 'floor'))],
    );
  }
  const ids = sorted.map(({ id }) => id);
  // Admission and model calls can fall in different budget periods.
  const lockIds = [...new Set([...ids, ...admission.flatMap(({ bucket_ids }) => bucket_ids)])];
  await lockBuckets(client, lockIds);
  for (const row of admission) {
    await client.query('UPDATE ledger_buckets SET reserved_usd = reserved_usd - $2 WHERE id = ANY($1::text[])',
      [row.bucket_ids, row.amount_usd]);
    await client.query("UPDATE ledger_reservations SET status = 'released', settled_at = now(), actual_usd = 0 WHERE id = $1",
      [row.id]);
  }
  const locked = new Map((await lockBuckets(client, ids)).map((row) => [row.id, row]));
  for (const bucket of buckets) {
    const row = locked.get(bucket.id);
    if (!row) throw new Error('reservation bucket missing');
    const cap = bucket.cap === null ? null : micros(bucket.cap, 'floor');
    if (cap !== null) {
      const available = cap - micros(row.reserved_usd) - micros(row.spent_usd) - protectedRoom;
      if (available < floor) throw new CapExceeded(bucket.level);
      requested = Math.min(requested, available);
    }
    if (row.cap_usd !== (cap === null ? null : dollars(cap))) {
      await client.query('UPDATE ledger_buckets SET cap_usd = $2 WHERE id = $1',
        [row.id, cap === null ? null : dollars(cap)]);
    }
  }
  await client.query(
    'UPDATE ledger_buckets SET reserved_usd = reserved_usd + $2 WHERE id = ANY($1::text[])', [ids, dollars(requested)],
  );
  const id = randomUUID();
  await client.query(
    `INSERT INTO ledger_reservations(id, step_run_id, group_id, runner_id, bucket_ids, kind, amount_usd)
     VALUES ($1,$2,$3,$4,$5,$6,$7)`,
    [id, context.stepId, context.groupId, context.runnerId, JSON.stringify(ids), kind, dollars(requested)],
  );
  return { reservation_id: id, ...(minimum !== undefined && { amount_usd: requested / 1_000_000 }) };
}
