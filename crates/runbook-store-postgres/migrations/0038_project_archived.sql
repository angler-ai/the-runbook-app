-- Archived projects (2026-11-18): hidden in the app and read-only (no new
-- runs, edits or triggers); everything is kept, and an org owner or admin
-- restores them.
ALTER TABLE runbook.project ADD COLUMN archived boolean NOT NULL DEFAULT false;
