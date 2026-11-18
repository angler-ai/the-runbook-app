-- F46: a purged run keeps its step-run IDs beside its tombstone, so a log
-- link of an expired run answers 410 execution.history_expired, not 404.
CREATE TABLE runbook.step_run_tombstone (
    step_run_id text PRIMARY KEY,
    execution_id text NOT NULL,
    org_id text NOT NULL,
    project_id text NOT NULL
);
CREATE INDEX step_run_tombstone_execution ON runbook.step_run_tombstone (execution_id);
