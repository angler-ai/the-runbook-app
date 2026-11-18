-- D1: 0002_execution_catalog.sql
-- Execution-owned tables (execution, execution_event, execution_timer,
-- execution_timer_history, execution_outbox, execution_idempotency,
-- execution_claim, execution_derived_outbox) are owned by ExecutionDO
-- (cloudflare/migrations/do/0001_execution.sql).
-- D1 holds the execution owner, catalog, admission and idempotency tables.

CREATE TABLE IF NOT EXISTS execution_owner (
    execution_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS execution_owner_scope ON execution_owner (org_id, project_id);

CREATE TABLE IF NOT EXISTS draft (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    key TEXT NOT NULL CHECK (key <> ''),
    revision INTEGER NOT NULL CHECK (revision > 0),
    source TEXT NOT NULL,
    PRIMARY KEY (org_id, project_id, key)
);

CREATE TABLE IF NOT EXISTS run_summary (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES execution_owner (execution_id) ON DELETE CASCADE,
    runbook_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    status TEXT NOT NULL,
    completed_at_ms INTEGER,
    body TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS assignment (
    execution_id TEXT NOT NULL REFERENCES execution_owner (execution_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    step_run_id TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (execution_id, step_run_id)
);

CREATE TABLE IF NOT EXISTS admission_pool (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    key TEXT NOT NULL CHECK (key <> ''),
    concurrency_limit INTEGER NOT NULL CHECK (concurrency_limit > 0),
    active INTEGER NOT NULL CHECK (active >= 0 AND active <= concurrency_limit),
    PRIMARY KEY (org_id, project_id, key)
);

CREATE TABLE IF NOT EXISTS admission (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES execution_owner (execution_id) ON DELETE CASCADE,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    key TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('admitted', 'queued', 'released')),
    queue_order INTEGER,
    request TEXT NOT NULL,
    CHECK ((state = 'queued') = (queue_order IS NOT NULL)),
    FOREIGN KEY (org_id, project_id, key) REFERENCES admission_pool (org_id, project_id, key)
);
CREATE INDEX IF NOT EXISTS admission_queue ON admission (org_id, project_id, key, queue_order)
    WHERE queue_order IS NOT NULL;

CREATE TABLE IF NOT EXISTS start_idempotency (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    execution_id TEXT NOT NULL REFERENCES execution_owner (execution_id) ON DELETE CASCADE,
    request TEXT NOT NULL,
    PRIMARY KEY (org_id, project_id, idempotency_key)
);
