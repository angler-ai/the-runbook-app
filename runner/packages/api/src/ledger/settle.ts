import type pg from 'pg';
import type { SettleRequest } from '@runbook/runner-shared/contracts/internal';
import { withTransaction } from '@runbook/runner-shared/db';
import { HttpError } from '@runbook/runner-shared/http';
import { crossedThresholds, enqueueBudgetAlerts, type BudgetAlert } from './alerts.ts';
import { dollars, lockBuckets, micros } from './buckets.ts';

interface Reservation {
  step_run_id: string;
  runner_id: string;
  group_id: string;
  bucket_ids: string[];
  capability_hash: Buffer | null;
  kind: 'model' | 'compute' | 'admission';
  amount_usd: string;
  actual_usd: string | null;
  status: 'reserved' | 'settled' | 'released';
  usage_pending: boolean;
}

function usageCounters(request: SettleRequest): number[] {
  return ['input_tokens', 'output_tokens', 'cached_tokens', 'cache_write_tokens'].map((key) => {
    const count = request.usage[key] ?? 0;
    if (!Number.isSafeInteger(count) || count < 0) throw new HttpError(400, 'ledger.invalid_usage', 'invalid usage');
    return count;
  });
}

export async function settle(pool: pg.Pool, request: SettleRequest, kind?: 'model' | 'compute', capabilityHash?: Buffer | null): Promise<BudgetAlert[]> {
  return withTransaction(pool, (client) => settleInTransaction(client, request, kind, capabilityHash));
}

export async function settleInTransaction(client: pg.PoolClient, request: SettleRequest, kind?: 'model' | 'compute', capabilityHash?: Buffer | null): Promise<BudgetAlert[]> {
  const actual = micros(request.actual_usd, 'ceil');
  const counters = usageCounters(request);
  const { rows: [reservation] } = await client.query<Reservation>(
    'SELECT * FROM ledger_reservations WHERE id = $1 FOR UPDATE', [request.reservation_id],
  );
  if (!reservation || (kind && reservation.kind !== kind)) throw new HttpError(404, 'ledger.not_found', 'reservation unavailable');
  // Migration 0019 left old rows without a hash. Only the authenticated
  // caller for their reservation kind can use the legacy settlement.
  if (capabilityHash === null && reservation.capability_hash) {
    throw new HttpError(400, 'ledger.invalid_request', 'settlement capability is required');
  }
  if (capabilityHash && reservation.capability_hash && !reservation.capability_hash.equals(capabilityHash)) {
    throw new HttpError(403, 'ledger.reservation_denied', 'reservation unavailable');
  }
  if (reservation.status !== 'reserved') {
    if (reservation.actual_usd === dollars(actual)) return [];
    throw new HttpError(409, 'ledger.already_settled', 'reservation already settled');
  }
  if (reservation.kind === 'admission' && actual !== 0) {
    throw new HttpError(400, 'ledger.invalid_amount', 'invalid admission settlement');
  }
  const buckets = await lockBuckets(client, reservation.bucket_ids);
  if (buckets.length !== reservation.bucket_ids.length) throw new Error('reservation bucket missing');
  const alerts: BudgetAlert[] = [];
  for (const bucket of buckets) {
    const crossed = crossedThresholds(bucket, actual);
    alerts.push(...crossed);
    await client.query(
      `UPDATE ledger_buckets SET reserved_usd = reserved_usd - $2, spent_usd = spent_usd + $3,
       alerts_sent = $4 WHERE id = $1`,
      [bucket.id, reservation.amount_usd, dollars(actual),
        JSON.stringify([...bucket.alerts_sent, ...crossed.map(({ threshold }) => threshold)])],
    );
  }
  await client.query(
    "UPDATE ledger_reservations SET actual_usd = $2, status = 'settled', settled_at = now(), usage_pending = $3 WHERE id = $1",
    [request.reservation_id, dollars(actual), reservation.kind === 'model' && request.usage_pending === true],
  );
  if (reservation.kind !== 'admission') {
    const compute = ['vcpu_seconds', 'gib_seconds'].map((key) => {
      const seconds = request.usage[key] ?? 0;
      if (!Number.isFinite(seconds) || seconds < 0 || seconds > 1e9 || (seconds && reservation.kind !== 'compute')) {
        throw new HttpError(400, 'ledger.invalid_usage', 'invalid compute usage');
      }
      return seconds;
    });
    const project = buckets.find(({ level }) => level === 'project')?.key;
    const agent = buckets.find(({ level }) => level === 'agent')?.key;
    await client.query(
      `INSERT INTO usage_events(step_run_id, group_id, runner_id, project_id, agent_id, kind,
        input_tokens, output_tokens, cached_tokens, cache_write_tokens, cost_usd, vcpu_seconds, gib_seconds, reservation_id)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)`,
      [reservation.step_run_id, reservation.group_id, reservation.runner_id, project,
        agent, reservation.kind, ...counters, dollars(actual), ...compute, request.reservation_id],
    );
  }
  await enqueueBudgetAlerts(client, alerts, reservation.group_id, reservation.runner_id);
  return alerts;
}

export async function reconcile(pool: pg.Pool, request: SettleRequest,
  authorization: { stepId: string; runnerId: string; overrideHold?: boolean }): Promise<BudgetAlert[]> {
  const actual = micros(request.actual_usd, 'ceil');
  const counters = usageCounters(request);
  return withTransaction(pool, async (client) => {
    const { rows: [reservation] } = await client.query<Reservation>(
      "SELECT * FROM ledger_reservations WHERE id = $1 AND kind = 'model' FOR UPDATE", [request.reservation_id]);
    const matches = reservation && reservation.step_run_id === authorization.stepId &&
      reservation.runner_id === authorization.runnerId;
    if (!matches) throw new HttpError(403, 'ledger.reservation_denied', 'reservation unavailable');
    if (reservation.status !== 'settled' || reservation.actual_usd === null) {
      throw new HttpError(409, 'ledger.not_pending', 'reservation is not pending reconciliation');
    }
    const { rows: [event] } = await client.query<{ counts: string[] }>(
      `SELECT ARRAY[input_tokens,output_tokens,cached_tokens,cache_write_tokens]::text[] AS counts
       FROM usage_events WHERE reservation_id = $1`, [request.reservation_id]);
    if (!event) throw new HttpError(409, 'ledger.usage_unlinked', 'reservation has no linked usage event');
    if (!reservation.usage_pending) {
      if (reservation.actual_usd === dollars(actual) && counters.every((value, index) => String(value) === event.counts[index])) return [];
      throw new HttpError(409, 'ledger.not_pending', 'reservation is not pending reconciliation');
    }
    const aboveHold = actual > micros(reservation.amount_usd);
    if (aboveHold && authorization.overrideHold !== true) {
      throw new HttpError(409, 'ledger.above_hold', 'actual charge exceeds the original hold; use --override-hold to apply it');
    }
    const buckets = await lockBuckets(client, reservation.bucket_ids);
    if (buckets.length !== reservation.bucket_ids.length) throw new Error('reservation bucket missing');
    const delta = actual - micros(reservation.actual_usd);
    const alerts: BudgetAlert[] = [];
    for (const bucket of buckets) {
      const crossed = delta > 0 ? crossedThresholds(bucket, delta) : [];
      alerts.push(...crossed);
      await client.query('UPDATE ledger_buckets SET spent_usd = spent_usd + $2, alerts_sent = $3 WHERE id = $1',
        [bucket.id, dollars(delta), JSON.stringify([...bucket.alerts_sent, ...crossed.map(({ threshold }) => threshold)])]);
    }
    const { rows: [updated] } = await client.query<{ updated: boolean }>(
      'SELECT reconcile_pending_usage($1,$2,$3,$4,$5,$6,$7) AS updated',
      [request.reservation_id, dollars(actual), ...counters, authorization.overrideHold === true]);
    if (!updated?.updated) throw new HttpError(409, 'ledger.not_pending', 'reservation is not pending reconciliation');
    await client.query('UPDATE ledger_reservations SET actual_usd = $2, usage_pending = false WHERE id = $1',
      [request.reservation_id, dollars(actual)]);
    await enqueueBudgetAlerts(client, alerts, reservation.group_id, reservation.runner_id);
    return alerts;
  });
}
