-- D1-only (CF5): a run deletion's leftovers outside D1. A deletion commits
-- in one D1 batch (tombstone, owner row and its cascade) and records the
-- run here; the host then empties the run's ExecutionDO and its R2 prefix
-- and deletes this row. A row still here later is finished by the minute
-- cron, so a crash between the D1 commit and the purge strands nothing.
CREATE TABLE IF NOT EXISTS object_purge (
    execution_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    keep_uploads INTEGER NOT NULL CHECK (keep_uploads IN (0, 1)),
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS object_purge_created ON object_purge (created_at_ms, execution_id);
