-- D1: 0015_draft_source_bytes.sql
-- Allow arbitrary UTF-8 including NUL in unpublished drafts (draft_source as BLOB).

CREATE TABLE IF NOT EXISTS runbook_new (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    runbook_id TEXT PRIMARY KEY NOT NULL,
    slug TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('ui', 'git')),
    archived INTEGER NOT NULL,
    draft_source BLOB NOT NULL,
    draft_revision INTEGER NOT NULL CHECK (draft_revision > 0),
    UNIQUE (org_id, project_id, slug)
);
INSERT INTO runbook_new SELECT org_id, project_id, runbook_id, slug, origin, archived, CAST(draft_source AS BLOB), draft_revision FROM runbook;
DROP TABLE runbook;
ALTER TABLE runbook_new RENAME TO runbook;
