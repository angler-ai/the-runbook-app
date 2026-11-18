-- D1: 0018_start_replay_retention.sql
-- Start replay retention creation timestamp.

ALTER TABLE start_replay ADD COLUMN created_at_ms INTEGER NOT NULL DEFAULT 0;
