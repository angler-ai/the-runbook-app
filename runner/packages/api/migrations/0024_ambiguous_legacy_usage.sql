-- 0020 paired historical siblings by stable IDs, which cannot establish
-- which counters belong to which reservation when the settlement keys tie.
-- Preserve 0020's checksum and clear ambiguous links on still-pending rows.
-- Usage totals and the provisional reservation charge stay intact.
WITH legacy AS (
  SELECT id, step_run_id, group_id, runner_id, actual_usd, settled_at, usage_pending,
         count(*) OVER (PARTITION BY step_run_id, group_id, runner_id, actual_usd, settled_at) AS siblings
    FROM ledger_reservations
    WHERE status = 'settled' AND kind = 'model'
      AND settled_at <= (SELECT applied_at FROM runner_schema_migrations WHERE version = '0020_usage_reconciliation')
)
UPDATE usage_events u SET reservation_id = NULL
  FROM legacy r
  WHERE r.siblings > 1 AND r.usage_pending AND u.reservation_id = r.id AND u.kind = 'model'
    AND (u.step_run_id, u.group_id, u.runner_id, u.cost_usd, u.created_at)
      = (r.step_run_id, r.group_id, r.runner_id, r.actual_usd, r.settled_at);
