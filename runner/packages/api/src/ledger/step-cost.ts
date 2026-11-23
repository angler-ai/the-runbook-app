import type pg from 'pg';
import type { CapabilityVerifier } from '@runbook/runner-shared/capability';
import { withTransaction } from '@runbook/runner-shared/db';
import { HttpError, type Route } from '@runbook/runner-shared/http';
import { lockBuckets } from './buckets.ts';
import { settleInTransaction } from './settle.ts';

export function stepCostRoute(pool: pg.Pool, verifier: Pick<CapabilityVerifier, 'verifyCapability'>,
  wakeAlerts: () => void): Route {
  return {
    method: 'POST', path: '/internal/slots/{slot_id}/cost', callers: ['worker'],
    async handle({ body, runnerId, params }) {
      const denied = () => new HttpError(403, 'ledger.slot_unavailable', 'step cost unavailable');
      const capability = (body as { capability?: unknown } | null)?.capability;
      if (typeof capability !== 'string' || !runnerId) throw denied();
      const claims = await verifier.verifyCapability(capability);
      if (claims.runner_id !== runnerId) throw denied();
      const cost = await withTransaction(pool, async (client) => {
        const { rows: [slot] } = await client.query<{ id: string }>(
          `UPDATE slots SET metering_closed_at = COALESCE(metering_closed_at, now())
           WHERE id = $1 AND runner_id = $2 AND step_run_id = $3 AND claim_id = $4
            AND claim_generation = $5 AND expires_at > now()
            AND (released_at IS NULL OR sandbox_stopped_at IS NOT NULL)
           RETURNING id`,
          [params.slot_id, runnerId, claims.sub, claims.claim_id, claims.generation],
        );
        if (!slot) throw denied();
        // A gateway timeout can leave an in-flight provider call. Close
        // metering under the slot lock and account for unfinished reservations.
        const { rows: reservations } = await client.query<{ id: string; amount_usd: string; bucket_ids: string[] }>(
          `SELECT id, amount_usd, bucket_ids FROM ledger_reservations
           WHERE slot_id = $1
             AND status = 'reserved' AND kind IN ('model', 'compute') ORDER BY id FOR UPDATE`,
          [slot.id],
        );
        await lockBuckets(client, [...new Set(reservations.flatMap(({ bucket_ids }) => bucket_ids))]);
        for (const reservation of reservations) {
          await settleInTransaction(client, { reservation_id: reservation.id, actual_usd: Number(reservation.amount_usd), usage: {}, usage_pending: true });
        }
        const { rows: [usage] } = await client.query<Record<string, string>>(
          `SELECT coalesce(sum(cost_usd) FILTER (WHERE u.kind = 'model'), 0) AS model_usd,
             coalesce(sum(cost_usd) FILTER (WHERE u.kind = 'compute'), 0) AS compute_usd,
             coalesce(sum(input_tokens), 0) AS input_tokens, coalesce(sum(output_tokens), 0) AS output_tokens,
             coalesce(sum(cached_tokens), 0) AS cached_tokens
           FROM usage_events u JOIN ledger_reservations r ON r.id = u.reservation_id WHERE r.slot_id = $1`,
          [slot.id],
        );
        return Object.fromEntries(Object.entries(usage!).map(([key, value]) => [key, Number(value)]));
      });
      wakeAlerts();
      return { body: cost };
    },
  };
}
