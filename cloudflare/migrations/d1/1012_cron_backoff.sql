-- D1: 1012_cron_backoff.sql (gate fix, low)
-- The minute cron's object work (run purges, supersede notices) reread the
-- oldest page every pass, so items that kept failing hid the ones after
-- them. A failed item now counts its attempts and moves its due time
-- (`created_at_ms`, which the pass orders and filters by) one minute on,
-- doubling per attempt, up to an hour.
ALTER TABLE object_purge ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE supersede_notice ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
