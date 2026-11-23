ALTER TABLE ledger_buckets DROP CONSTRAINT ledger_buckets_level_check;
ALTER TABLE ledger_buckets ADD CONSTRAINT ledger_buckets_level_check
  CHECK (level IN ('step', 'run', 'agent', 'project', 'group', 'environment', 'org'));
ALTER TABLE ledger_buckets
  ADD COLUMN alert_threshold_percent integer CHECK (alert_threshold_percent BETWEEN 1 AND 100),
  ADD COLUMN configured_alert_sent boolean NOT NULL DEFAULT false;
-- Carry existing day spend and open holds into the new monthly scopes.
-- The identifiers use the same compact JSON tuple as budgetBuckets.
WITH monthly AS (
  SELECT level, key, date_trunc('month', period_start::timestamp)::date AS period_start,
    sum(spent_usd) AS spent_usd, sum(reserved_usd) AS reserved_usd
  FROM ledger_buckets WHERE level IN ('agent','project') AND period = 'day'
  GROUP BY 1, 2, 3
)
INSERT INTO ledger_buckets(id,level,key,period,period_start,spent_usd,reserved_usd)
SELECT encode(sha256(convert_to('[' || to_json(level)::text || ',' || to_json(key)::text ||
    ',"month",' || to_json(period_start)::text || ']', 'UTF8')), 'hex'),
  level,key,'month',period_start,spent_usd,reserved_usd
FROM monthly;

WITH links AS (
  SELECT r.id, jsonb_agg(month.id ORDER BY month.id) AS bucket_ids
  FROM ledger_reservations r
  CROSS JOIN LATERAL jsonb_array_elements_text(r.bucket_ids) AS bound(id)
  JOIN ledger_buckets day ON day.id = bound.id AND day.period = 'day' AND day.level IN ('agent','project')
  JOIN ledger_buckets month ON month.level = day.level AND month.key = day.key AND month.period = 'month'
    AND month.period_start = date_trunc('month', day.period_start::timestamp)::date
  WHERE r.status IN ('reserved','settled')
  GROUP BY r.id
)
UPDATE ledger_reservations r SET bucket_ids = r.bucket_ids || links.bucket_ids FROM links WHERE links.id = r.id;

ALTER TABLE policy
  ADD COLUMN alert_threshold_percent integer CHECK (alert_threshold_percent BETWEEN 1 AND 100),
  ALTER COLUMN org_cost_per_day_usd DROP NOT NULL,
  ALTER COLUMN org_concurrency DROP NOT NULL;
ALTER TABLE groups ADD COLUMN alert_threshold_percent integer CHECK (alert_threshold_percent BETWEEN 1 AND 100);
ALTER TABLE slots ADD COLUMN environment_id text REFERENCES environments(id);
CREATE INDEX slots_environment_live ON slots (environment_id) WHERE released_at IS NULL;
CREATE INDEX ledger_budget_history ON ledger_buckets (period, period_start, level, key);

CREATE TABLE budget_waiting (
  step_run_id text PRIMARY KEY,
  group_id text NOT NULL,
  project_id text NOT NULL,
  scope text NOT NULL,
  budget_key text NOT NULL,
  period text NOT NULL,
  detail text NOT NULL,
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX budget_waiting_group ON budget_waiting (group_id, updated_at);
GRANT SELECT ON budget_waiting TO rb_api;
GRANT SELECT, INSERT, UPDATE, DELETE ON budget_waiting TO rb_scheduler;
