-- D1: 0014_start_admission.sql
-- Admission timestamps, pool limit adjustment, and start replay.
-- Note: execution_start is owned by ExecutionDO as `start_record` (0001_execution.sql).

ALTER TABLE admission ADD COLUMN admitted_at_ms INTEGER;

CREATE TABLE IF NOT EXISTS admission_pool_new (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    key TEXT NOT NULL CHECK (key <> ''),
    concurrency_limit INTEGER NOT NULL CHECK (concurrency_limit > 0),
    active INTEGER NOT NULL CHECK (active >= 0),
    PRIMARY KEY (org_id, project_id, key)
);
INSERT INTO admission_pool_new SELECT org_id, project_id, key, concurrency_limit, active FROM admission_pool;
DROP TABLE admission_pool;
ALTER TABLE admission_pool_new RENAME TO admission_pool;

CREATE TABLE IF NOT EXISTS start_replay (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    principal TEXT NOT NULL,
    request_hash BLOB NOT NULL CHECK (length(request_hash) = 32),
    execution_id TEXT NOT NULL REFERENCES execution_owner (execution_id) ON DELETE CASCADE,
    outcome TEXT NOT NULL,
    PRIMARY KEY (org_id, project_id, idempotency_key)
);
