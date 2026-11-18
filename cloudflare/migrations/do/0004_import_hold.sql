-- ExecutionDO 0004 (gate fix D2): a run a portable import installed stays
-- held until the import's D1 batch commits.
--
-- While the row exists the object arms no alarm and hands out no derived
-- rows or notifications, so nothing settles, drains into D1 or publishes
-- before the import's commit point. The importer releases each run after
-- the commit (D1 `run_release`, which the minute cron finishes); a failed
-- import purges the object instead.
CREATE TABLE IF NOT EXISTS import_hold (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    held_at_ms INTEGER NOT NULL
);
