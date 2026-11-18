-- OrgDO 0001 (CF6): one organization's execution quota (PRD §17, D25).
-- The tables and columns are Postgres's 0030_quotas_usage, so the parity
-- test compares them. One row per month, created by its first start (the
-- month rolls over lazily); one reservation per counted start, so a retry
-- counts once and a refund (a run cancelled while queued, or a start whose
-- D1 batch failed) finds its month.
CREATE TABLE usage_monthly (
  org_id TEXT NOT NULL,
  month TEXT NOT NULL,
  executions_started INTEGER NOT NULL CHECK (executions_started >= 0),
  PRIMARY KEY (org_id, month)
);
CREATE TABLE quota_reservation (
  execution_id TEXT PRIMARY KEY NOT NULL,
  org_id TEXT NOT NULL,
  month TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL
);
CREATE INDEX quota_reservation_created ON quota_reservation (created_at_ms, execution_id);
