ALTER TABLE usage_events ADD COLUMN reservation_id text REFERENCES ledger_reservations(id);
CREATE UNIQUE INDEX usage_events_reservation_idx ON usage_events(reservation_id);

-- Previous settlement wrote both rows in one transaction using now().
-- Pair events only where every row in the settlement group can be accounted for.
WITH reservations AS (
  SELECT id, step_run_id, group_id, runner_id, kind, actual_usd, settled_at,
         row_number() OVER settlement AS position, count(*) OVER settlement AS size
    FROM ledger_reservations WHERE status = 'settled' AND kind = 'model'
    WINDOW settlement AS (PARTITION BY step_run_id, group_id, runner_id, kind, actual_usd, settled_at
                          ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)
), events AS (
  SELECT id, step_run_id, group_id, runner_id, kind, cost_usd, created_at,
         row_number() OVER settlement AS position, count(*) OVER settlement AS size
    FROM usage_events WHERE kind = 'model'
    WINDOW settlement AS (PARTITION BY step_run_id, group_id, runner_id, kind, cost_usd, created_at
                          ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)
)
UPDATE usage_events u SET reservation_id = r.id
  FROM reservations r JOIN events e
    ON (r.step_run_id, r.group_id, r.runner_id, r.kind, r.actual_usd, r.settled_at, r.position, r.size)
     = (e.step_run_id, e.group_id, e.runner_id, e.kind, e.cost_usd, e.created_at, e.position, e.size)
  WHERE u.id = e.id;

GRANT UPDATE (input_tokens, output_tokens, cached_tokens, cache_write_tokens, cost_usd)
  ON usage_events TO rb_scheduler;
