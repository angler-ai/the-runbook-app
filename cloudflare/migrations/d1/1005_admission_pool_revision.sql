-- D1-only (CF3 review fix 3): a revision on each admission pool.
-- Admission and release are guarded batches over what they read. Guarding
-- the pool's limit and active count was not enough: a queued start rewrites
-- the same values, so a release planned before it still passed its guard
-- and freed the slot without promoting the new waiter. Every admission or
-- release batch bumps this revision and guards the one it read.
ALTER TABLE admission_pool ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
