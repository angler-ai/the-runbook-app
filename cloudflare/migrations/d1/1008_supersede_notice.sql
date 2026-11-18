-- D1-only (CF5): LANG3 `cancel_previous` on Cloudflare. A start's D1 batch
-- decides the pool (evicted waiters released, the start admitted or queued
-- first in line) and records here each run it evicts; the starting host
-- then has each evicted run's object prepare and commit its cancellation
-- (`superseded_by`, `started_by`) and deletes the row. A row still here is
-- delivered by the minute cron. An evicted holder keeps its slot until its
-- cancellation closes it; the applier's release then promotes the start.
CREATE TABLE IF NOT EXISTS supersede_notice (
    execution_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('admitted', 'queued')),
    superseded_by TEXT NOT NULL,
    started_by TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS supersede_notice_created
    ON supersede_notice (created_at_ms, execution_id);
