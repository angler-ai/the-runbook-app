-- D1: 1015_replay_sweep_backoff.sql (CF12b review round 2)
-- A replay row whose R2 object delete failed backs off, so later sweeps move
-- past it: `sweep_after` is when it may be tried again (NULL: now), an hour
-- after its first failure, doubling per failure (`sweep_failures`), up to a
-- week. Older code neither reads nor writes these columns.
ALTER TABLE directory_request ADD COLUMN sweep_after INTEGER;
ALTER TABLE directory_request ADD COLUMN sweep_failures INTEGER NOT NULL DEFAULT 0;
