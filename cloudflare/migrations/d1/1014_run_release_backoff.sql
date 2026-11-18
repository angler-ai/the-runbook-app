-- D1: 1014_run_release_backoff.sql (gate round 2)
-- A run release the minute cron could not finish backs off on its own, as
-- a purge does (1012): its due time (`created_at_ms`) moves a minute on,
-- doubling per attempt, up to an hour. `run_release` rows now commit with
-- an import's control batch (the control database), so they exist only for
-- a fully committed import.
ALTER TABLE run_release ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
