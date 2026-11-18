-- D1: 0025_inbox_index.sql
-- Task inbox query projection columns on assignment table.

ALTER TABLE assignment ADD COLUMN kind TEXT NOT NULL DEFAULT 'work';
ALTER TABLE assignment ADD COLUMN updated_at_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE assignment ADD COLUMN recipients TEXT NOT NULL DEFAULT '[]';
ALTER TABLE assignment ADD COLUMN voted_by TEXT NOT NULL DEFAULT '[]';

CREATE INDEX IF NOT EXISTS assignment_inbox_order ON assignment
    (updated_at_ms DESC, execution_id DESC, step_run_id DESC);
