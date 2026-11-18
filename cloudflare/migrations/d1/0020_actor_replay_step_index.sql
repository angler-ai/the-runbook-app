-- D1: 0020_actor_replay_step_index.sql
-- Step run index for actor targeting across executions.
-- actor_replay is owned by ExecutionDO (cloudflare/migrations/do/0001_execution.sql).

CREATE TABLE IF NOT EXISTS step_run_index (
    step_run_id TEXT PRIMARY KEY NOT NULL,
    execution_id TEXT NOT NULL REFERENCES execution_owner (execution_id) ON DELETE CASCADE,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS step_run_index_execution ON step_run_index (execution_id);
