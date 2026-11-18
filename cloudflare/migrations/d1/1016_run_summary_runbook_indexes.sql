-- D1: 1016_run_summary_runbook_indexes.sql (UI-1 review)
-- The runbook list reads, for one page of runbooks in one query, each
-- runbook's newest run and how many of its runs are open
-- (crates/runbook-store-d1/src/search.rs, RUNBOOK_RUN_STATS_SQL), as
-- Postgres 0032 does. Without these, each lookup scanned the tenant's
-- execution_owner rows.
-- The newest run: one probe in D22 order (created time, then execution ID).
CREATE INDEX IF NOT EXISTS run_summary_runbook_created
    ON run_summary (runbook_id, created_at_ms DESC, execution_id DESC);
-- Open runs only. SQLite uses a partial index only when the query repeats
-- its WHERE terms, so the query names this status list verbatim.
CREATE INDEX IF NOT EXISTS run_summary_runbook_active ON run_summary (runbook_id)
    WHERE status IN ('queued', 'running', 'waiting', 'paused');
