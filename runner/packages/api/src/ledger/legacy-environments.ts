import type pg from 'pg';
import { withTransaction } from '@runbook/runner-shared/db';
import { HttpError } from '@runbook/runner-shared/http';
import { lockBudgetSettings } from './limits.ts';

// An installation can keep pre-upgrade sandboxes alive while the API upgrades.
export async function attributeLegacySlots(pool: pg.Pool, orgId: string, required = false) {
  await withTransaction(pool, async (client) => {
    await lockBudgetSettings(client);
    const { rows: [environment] } = await client.query<{ id: string }>(
      'SELECT id FROM environments WHERE org_id=$1 AND is_default', [orgId]);
    if (!environment) {
      if (required) throw new HttpError(503, 'environment.default_unavailable', 'sandbox environment unavailable');
      return;
    }
    const { rows: slots } = await client.query<{ id: string }>(
      `UPDATE slots SET environment_id=$1 WHERE environment_id IS NULL AND released_at IS NULL RETURNING id`,
      [environment.id]);
    if (!slots.length) return;
    await client.query(
      `WITH holds AS MATERIALIZED (
        SELECT r.id, r.bucket_ids, r.status, r.amount_usd, r.actual_usd - r.overrun_usd AS actual_usd, r.overrun_usd
        FROM ledger_reservations r WHERE r.status IN ('reserved','settled') AND EXISTS (
          SELECT 1 FROM slots s WHERE s.id=ANY($2::text[]) AND (
            r.slot_id=s.id OR r.id=s.admission_reservation_id OR
            r.slot_id IS NULL AND r.decision_lease_id=s.claim_id AND r.runner_id=s.runner_id AND r.step_run_id=s.step_run_id
          )
        ) ORDER BY r.id FOR UPDATE
      ), links AS MATERIALIZED (
        SELECT holds.*, b.period, b.period_start,
          encode(sha256(convert_to('["environment",' || to_json($1::text)::text || ',' ||
            to_json(b.period)::text || ',' || to_json(b.period_start)::text || ']', 'UTF8')), 'hex') AS bucket_id
        FROM holds CROSS JOIN LATERAL jsonb_array_elements_text(holds.bucket_ids) AS bound(id)
        JOIN ledger_buckets b ON b.id=bound.id AND b.level='org' AND b.period IN ('day','month')
      ), buckets AS (
        INSERT INTO ledger_buckets(id,level,key,period,period_start,reserved_usd,spent_usd,overrun_usd)
        SELECT bucket_id,'environment',$1,period,period_start,
          COALESCE(sum(amount_usd) FILTER (WHERE status='reserved'),0),
          COALESCE(sum(actual_usd) FILTER (WHERE status='settled'),0),
          COALESCE(sum(overrun_usd) FILTER (WHERE status='settled'),0)
        FROM links GROUP BY bucket_id,period,period_start ORDER BY bucket_id
        ON CONFLICT(id) DO UPDATE SET reserved_usd=ledger_buckets.reserved_usd+EXCLUDED.reserved_usd,
          spent_usd=ledger_buckets.spent_usd+EXCLUDED.spent_usd,
          overrun_usd=ledger_buckets.overrun_usd+EXCLUDED.overrun_usd
        RETURNING id
      ), attachments AS (
        SELECT links.id, jsonb_agg(bucket_id ORDER BY bucket_id) AS ids
        FROM links JOIN buckets ON buckets.id=links.bucket_id GROUP BY links.id
      )
      UPDATE ledger_reservations r SET bucket_ids=r.bucket_ids || attachments.ids
      FROM attachments WHERE r.id=attachments.id`,
      [environment.id, slots.map(({ id }) => id)]);
  });
}
