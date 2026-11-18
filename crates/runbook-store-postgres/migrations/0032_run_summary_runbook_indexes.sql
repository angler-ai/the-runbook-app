-- UI-1 review: the runbook list reads, for one page of runbooks in one
-- query, each runbook's newest run and how many of its runs are open
-- (crates/runbook-store-postgres/src/search.rs, RUNBOOK_RUN_STATS_SQL).
-- Without these, each lookup scanned run_summary or the tenant's owners.
-- The newest run: one probe in D22 order (created time, then execution ID).
CREATE INDEX run_summary_runbook_created
    ON runbook.run_summary (runbook_id, created_at_ms DESC, execution_id DESC);
-- Open runs only: small however many runs a runbook has finished. The
-- query repeats this status list verbatim, so the planner can use it.
CREATE INDEX run_summary_runbook_active ON runbook.run_summary (runbook_id)
    WHERE status IN ('queued', 'running', 'waiting', 'paused');
