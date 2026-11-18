-- D1: 1011_run_release.sql (gate fix D2)
-- A portable import installs each run into its ExecutionDO held (no alarm,
-- no derived rows, no notifications), commits its D1 batch, then releases
-- each run. The batch writes one row per run here; the importer deletes it
-- once the run's object is released, and the minute cron releases the runs
-- of an import that stopped in between.
CREATE TABLE IF NOT EXISTS run_release (
    execution_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS run_release_age ON run_release (created_at_ms, execution_id);
