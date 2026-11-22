import { randomUUID } from 'node:crypto';
import type pg from 'pg';
import type { GrantClaims, ReadyItem, WaitingReason } from '@runbook/runner-shared/contracts/core';
import type { SlotOutcome } from '@runbook/runner-shared/contracts/internal';
import { sandboxIdForClaim } from '@runbook/runner-shared/capability';
import { withTransaction } from '@runbook/runner-shared/db';
import { settleInTransaction } from '../ledger/settle.ts';
import { lockBuckets, type BudgetContext } from '../ledger/buckets.ts';
import { CapExceeded, reserveInTransaction } from '../ledger/reserve.ts';

const levels = ['org', 'group', 'project', 'agent', 'runner'] as const;
type Level = (typeof levels)[number];
type Counts = Record<Level, number>;
type Limits = Record<Level, number | null>;

export const defaultLimits = { orgConcurrency: 20, orgDailyUsd: 50, agentConcurrency: 5, stepUsd: 1 };

export function checkConcurrency(current: Counts, limits: Limits): WaitingReason | undefined {
  for (const level of levels) {
    const cap = limits[level];
    if (cap !== null && current[level] >= cap) return `limit:${level}`;
  }
}

export function admissionAmount(stepCap: number): number {
  return Math.min(0.25, stepCap);
}

export function liveSlot(slot: { expiresAt: number }, millis: number): boolean {
  return slot.expiresAt > millis;
}

const admissionLock = 7419003;

async function releaseLocked(client: pg.PoolClient, clause: 'expired' | 'named' | 'sandbox', params: (string | Date)[]) {
  const filters = {
    expired: `(claim_id IS NULL AND expires_at <= $1) OR
      (expires_at <= $1::timestamptz - interval '60 seconds' AND
        (sandbox_started_at IS NULL OR sandbox_stopped_at IS NOT NULL))`,
    named: 'id = $1 AND runner_id = $2',
    sandbox: 'sandbox_id = $1',
  };
  const where = filters[clause];
  const { rows } = await client.query<{ id: string }>(
    `UPDATE slots SET released_at = now(), release_reason = '${clause === 'named' ? 'released' : 'lease_expired'}'
     WHERE released_at IS NULL AND (${where}) RETURNING id`, params,
  );
  const reservations = await client.query<{
    id: string; bucket_ids: string[]; amount_usd: string; kind: 'model' | 'compute' | 'admission';
  }>(
    `SELECT r.id, r.bucket_ids, r.amount_usd, r.kind FROM ledger_reservations r
     WHERE r.status = 'reserved' AND EXISTS (
       SELECT 1 FROM slots s WHERE s.id = ANY($1::text[]) AND (
         s.admission_reservation_id = r.id OR r.slot_id = s.id OR (
           r.slot_id IS NULL AND r.decision_lease_id = s.claim_id
           AND r.step_run_id = s.step_run_id AND r.runner_id = s.runner_id
         )
       )
     ) ORDER BY r.id FOR UPDATE`, [rows.map(({ id }) => id)],
  );
  await lockBuckets(client, [...new Set(reservations.rows.flatMap(({ bucket_ids }) => bucket_ids))]);
  for (const reservation of reservations.rows) {
    if (reservation.kind !== 'admission') {
      await settleInTransaction(client, { reservation_id: reservation.id, actual_usd: Number(reservation.amount_usd), usage: {},
        usage_pending: reservation.kind === 'model' });
      continue;
    }
    await client.query('UPDATE ledger_buckets SET reserved_usd = reserved_usd - $2 WHERE id = ANY($1::text[])',
      [reservation.bucket_ids, reservation.amount_usd]);
    await client.query("UPDATE ledger_reservations SET status = 'released', settled_at = now(), actual_usd = 0 WHERE id = $1",
      [reservation.id]);
  }
  // Closing the slot and enqueuing its final ledger total are atomic.
  // Always enqueue spend: a successful report may have omitted its cost,
  // and core deduplicates a cost it already received for this claim.
  await client.query(
    `INSERT INTO budget_event_outbox(id, group_id, runner_id, kind, data)
     SELECT (CASE WHEN r.decision_lease_id IS NULL THEN 'task-cost-' ELSE 'decision-cost-' END) || s.claim_id,
       s.group_id, s.runner_id,
       CASE WHEN r.decision_lease_id IS NULL THEN 'runner.task_cost' ELSE 'runner.decision_cost' END,
       jsonb_build_object('step_run_id', s.step_run_id, 'cost',
         jsonb_build_object('model_usd', coalesce(sum(u.cost_usd) FILTER (WHERE u.kind = 'model'), 0),
           'compute_usd', coalesce(sum(u.cost_usd) FILTER (WHERE u.kind = 'compute'), 0),
           'input_tokens', coalesce(sum(u.input_tokens), 0), 'output_tokens', coalesce(sum(u.output_tokens), 0),
           'cached_tokens', coalesce(sum(u.cached_tokens), 0)))
       || CASE WHEN r.decision_lease_id IS NULL THEN jsonb_build_object('claim_id', s.claim_id, 'runner_id', s.runner_id)
               ELSE jsonb_build_object('lease_id', s.claim_id) END
     FROM slots s JOIN ledger_reservations r ON r.slot_id = s.id OR (
       r.slot_id IS NULL AND r.decision_lease_id = s.claim_id
       AND r.step_run_id = s.step_run_id AND r.runner_id = s.runner_id)
     JOIN usage_events u ON u.reservation_id = r.id
     WHERE s.id = ANY($1::text[]) AND s.claim_id IS NOT NULL
     GROUP BY s.claim_id, s.step_run_id, s.group_id, s.runner_id, r.decision_lease_id
     ON CONFLICT (id) DO NOTHING`,
    [rows.map(({ id }) => id)],
  );
  return rows.length;
}

export async function releaseSlot(pool: pg.Pool, runnerId: string, slotId: string, outcome?: SlotOutcome): Promise<boolean> {
  return withTransaction(pool, async (client) => {
    await client.query('SELECT pg_advisory_xact_lock($1)', [admissionLock]);
    if (outcome) {
      const { rowCount } = await client.query(
        `UPDATE slots SET outcome = COALESCE(outcome, $5),
          idle_stop = CASE WHEN outcome IS NULL THEN $6 ELSE idle_stop END,
          reported_at = COALESCE(reported_at, now())
         WHERE id = $1 AND runner_id = $2 AND claim_id = $3 AND claim_generation = $4`,
        [slotId, runnerId, outcome.claim_id, outcome.generation, outcome.status, outcome.idle_stop],
      );
      if (!rowCount) return false;
    }
    return (await releaseLocked(client, 'named', [slotId, runnerId])) === 1 || outcome !== undefined;
  });
}

export async function releaseSandboxSlot(pool: pg.Pool, sandboxId: string): Promise<void> {
  await withTransaction(pool, async (client) => {
    await client.query('SELECT pg_advisory_xact_lock($1)', [admissionLock]);
    await client.query(
      `UPDATE slots SET sandbox_stopped_at = COALESCE(sandbox_stopped_at, now())
       WHERE sandbox_id = $1 AND sandbox_started_at IS NOT NULL`, [sandboxId],
    );
    await releaseLocked(client, 'sandbox', [sandboxId]);
  });
}

type SlotClaim = Pick<GrantClaims, 'sub' | 'runner_id' | 'group_id' | 'project_id' | 'agent_id' | 'claim_id' | 'generation' | 'exp' | 'agent_version'>;

export async function refreshSlot(pool: pg.Pool, slotId: string, claims: SlotClaim, now = new Date()): Promise<boolean> {
  const expiry = new Date(claims.exp * 1000);
  if (expiry <= now) return false;
  // A heartbeat moves the slot deadline with the claim. Live sandboxes still require confirmed teardown.
  const result = await pool.query(
    `UPDATE slots SET claim_id = $7, claim_generation = $8, sandbox_id = $11,
       claimed_at = COALESCE(claimed_at, $10), agent_version_id = $12,
       expires_at = CASE WHEN claim_id IS NULL THEN $9::timestamptz ELSE GREATEST(expires_at, $9) END
     WHERE id = $1 AND step_run_id = $2 AND runner_id = $3 AND group_id = $4 AND project_id = $5
       AND agent_id = $6 AND released_at IS NULL AND (agent_version_id IS NULL OR agent_version_id = $12) AND
       ((claim_id IS NULL AND expires_at > $10) OR (claim_id = $7 AND claim_generation = $8))`,
    [slotId, claims.sub, claims.runner_id, claims.group_id, claims.project_id, claims.agent_id,
      claims.claim_id, claims.generation, expiry, now, sandboxIdForClaim(claims), claims.agent_version],
  );
  return result.rowCount === 1;
}

export async function reapSlots(pool: pg.Pool, now = new Date()): Promise<number> {
  return withTransaction(pool, async (client) => {
    await client.query('SELECT pg_advisory_xact_lock($1)', [admissionLock]);
    return releaseLocked(client, 'expired', [now]);
  });
}

export async function admit(pool: pg.Pool, item: ReadyItem, context: BudgetContext, limits: Limits,
  expiresAt: Date, now = new Date()): Promise<{ slot_id: string } | { reason: WaitingReason } | undefined> {
  if (!liveSlot({ expiresAt: expiresAt.getTime() }, now.getTime())) return undefined;
  try {
    return await withTransaction(pool, async (client) => {
      await client.query('SELECT pg_advisory_xact_lock($1)', [admissionLock]);
      const duplicate = await client.query('SELECT 1 FROM slots WHERE step_run_id = $1 AND released_at IS NULL', [item.step_run_id]);
      if (duplicate.rowCount) return undefined;
      const { rows: [counts] } = await client.query<Counts>(
        `SELECT count(*)::int AS org,
           count(*) FILTER (WHERE group_id = $1)::int AS "group",
           count(*) FILTER (WHERE project_id = $2 AND group_id = $1)::int AS project,
           count(*) FILTER (WHERE agent_id = $3)::int AS agent,
           count(*) FILTER (WHERE runner_id = $4)::int AS runner
         FROM slots WHERE released_at IS NULL`,
        [context.groupId, context.projectId, context.agentId, context.runnerId],
      );
      const reason = checkConcurrency(counts!, limits);
      if (reason) return { reason };
      const id = randomUUID();
      const reservation = await reserveInTransaction(client, context,
        admissionAmount(context.caps.step ?? defaultLimits.stepUsd), 'admission', now);
      await client.query(
        `INSERT INTO slots(id, step_run_id, group_id, runner_id, project_id, agent_id,
          execution_id, expires_at, admission_reservation_id, max_cost_usd)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)`,
        [id, item.step_run_id, context.groupId, context.runnerId, context.projectId, context.agentId,
          item.execution_id, expiresAt, reservation.reservation_id, context.caps.step],
      );
      return { slot_id: id };
    });
  } catch (error) {
    if (error instanceof CapExceeded) return { reason: `limit:${error.level}` };
    throw error;
  }
}
