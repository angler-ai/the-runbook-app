CREATE TABLE work_event_receipts (
  id text PRIMARY KEY,
  received_at timestamptz NOT NULL
);
CREATE INDEX work_event_receipts_time ON work_event_receipts (received_at);

CREATE TABLE budget_event_outbox (
  id text PRIMARY KEY,
  group_id text NOT NULL,
  runner_id text NOT NULL,
  data jsonb NOT NULL,
  attempts integer NOT NULL DEFAULT 0,
  next_attempt_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX budget_event_outbox_due ON budget_event_outbox (next_attempt_at);
GRANT SELECT, INSERT, DELETE ON work_event_receipts TO rb_scheduler;
GRANT SELECT, INSERT, UPDATE, DELETE ON budget_event_outbox TO rb_scheduler;
