-- B5a follow-up (D20): start replay rows carry their creation time so the
-- 24-hour retention window can expire them. Rows predating this column
-- read as expired.
ALTER TABLE runbook.start_replay
    ADD COLUMN created_at_ms bigint NOT NULL DEFAULT 0;
