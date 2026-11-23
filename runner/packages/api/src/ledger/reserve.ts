import { randomUUID } from 'node:crypto';
import type pg from 'pg';
import { withTransaction } from '@runbook/runner-shared/db';
import { budgetBuckets, dollars, lockBuckets, micros, type BudgetContext, type Level, type Period } from './buckets.ts';

export class CapExceeded extends Error {
  readonly level: Level;
  readonly period: Period;
  readonly key: string;
  readonly cap: number;
  readonly close?: { id: string; capUsd: string };
  constructor(level: Level, period: Period, key: string, cap: number, close?: { id: string; capUsd: string }) {
    super('budget reached');
    this.level = level;
    this.period = period;
    this.key = key;
    this.cap = cap;
    this.close = close;
  }
}

export async function reserve(pool: pg.Pool, context: BudgetContext, amount: number,
  kind: 'model' | 'compute' | 'admission', now = new Date()): Promise<{ reservation_id: string } | { level: string }> {
  const result = await withTransaction(pool, (client) => tryReserve(client, context, amount, kind, now));
  return result instanceof CapExceeded ? { level: result.level } : result;
}

// Roll back the attempted hold without discarding an observation that a
// shared period has already exhausted its authoritative cap.
export async function tryReserve(...args: Parameters<typeof reserveInTransaction>) {
  const [client] = args;
  await client.query('SAVEPOINT budget_hold');
  try {
    const held = await reserveInTransaction(...args);
    await client.query('RELEASE SAVEPOINT budget_hold');
    return held;
  } catch (error) {
    if (!(error instanceof CapExceeded)) throw error;
    await client.query('ROLLBACK TO SAVEPOINT budget_hold');
    await client.query('RELEASE SAVEPOINT budget_hold');
    if (error.close) {
      // Only an existing bucket can already have spent funds.
      await client.query('UPDATE ledger_buckets SET closed_cap_usd=LEAST(closed_cap_usd,$2::numeric) WHERE id=$1',
        [error.close.id, error.close.capUsd]);
    }
    return error;
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
      `INSERT INTO ledger_buckets(id, level, key, period, period_start, group_id, runner_id, cap_usd, alert_threshold_percent)
       VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING`,
      [row.id, row.level, row.key, row.period, row.start,
        ['org', 'project', 'agent', 'run', 'environment'].includes(row.level) ? null : context.groupId,
        row.level === 'step' ? context.runnerId : null,
        row.cap === null ? null : dollars(micros(row.cap, 'floor')), row.threshold],
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
    const pinned = bucket.pinned === undefined ? Infinity : micros(bucket.pinned, 'floor');
    const current = bucket.cap === null ? Infinity : micros(bucket.cap, 'floor');
    const configured = bucket.level === 'run' ? Math.min(current, pinned) : current;
    // Every step in an execution shares the run bucket. A different agent
    // can introduce a narrower run budget, but cannot reopen existing room.
    const previous = row.cap_usd === null ? null : micros(row.cap_usd);
    const spent = micros(row.spent_usd) + micros(row.overrun_usd ?? '0');
    const shared = bucket.level === 'agent' || bucket.level === 'environment';
    const closed = row.closed_cap_usd == null ? Infinity : micros(row.closed_cap_usd);
    // The old cap covers previously closed periods and, for runs, keeps the
    // lowest ceiling of all their admitted steps.
    const retained = previous !== null && (bucket.level === 'run' || (shared && spent >= previous)) ? previous : Infinity;
    const allowed = Math.min(configured, retained, closed);
    const cap = Number.isFinite(allowed) ? allowed : null;
    const ceiling = Math.min(allowed, pinned);
    if (Number.isFinite(ceiling)) {
      const available = ceiling - micros(row.reserved_usd) - spent - protectedRoom;
      const close = shared && cap !== null && spent >= cap ? { id: row.id, capUsd: dollars(cap) } : undefined;
      if (available < floor || (closed < Infinity && requested > 0 && floor > 0))
        throw new CapExceeded(bucket.level, bucket.period, bucket.key, ceiling / 1_000_000, close);
      requested = Math.min(requested, closed < Infinity ? 0 : available);
    }
    const capUsd = cap === null ? null : dollars(cap);
    if (row.cap_usd !== capUsd || row.alert_threshold_percent !== bucket.threshold) {
      await client.query('UPDATE ledger_buckets SET cap_usd = $2, alert_threshold_percent = $3 WHERE id = $1',
        [row.id, capUsd, bucket.threshold]);
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
