-- D1: 0024_step_run_tombstone.sql
-- Step run tombstones for 410 history_expired responses on purged runs.

CREATE TABLE IF NOT EXISTS step_run_tombstone (
    step_run_id TEXT PRIMARY KEY NOT NULL,
    execution_id TEXT NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS step_run_tombstone_execution ON step_run_tombstone (execution_id);
