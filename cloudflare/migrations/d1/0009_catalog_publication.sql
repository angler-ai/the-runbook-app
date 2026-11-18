-- D1: 0009_catalog_publication.sql
-- Runbook identity and immutable published versions.

CREATE TABLE IF NOT EXISTS runbook (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    runbook_id TEXT PRIMARY KEY NOT NULL,
    slug TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('ui', 'git')),
    archived INTEGER NOT NULL,
    draft_source TEXT NOT NULL,
    draft_revision INTEGER NOT NULL CHECK (draft_revision > 0),
    UNIQUE (org_id, project_id, slug)
);

CREATE TABLE IF NOT EXISTS runbook_version (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    runbook_id TEXT NOT NULL REFERENCES runbook (runbook_id) ON DELETE CASCADE,
    version_id TEXT PRIMARY KEY NOT NULL,
    number INTEGER CHECK (number > 0),
    branch TEXT,
    commit_sha TEXT,
    semantic_hash BLOB NOT NULL CHECK (length(semantic_hash) = 32),
    snapshot_digest BLOB NOT NULL CHECK (length(snapshot_digest) = 32),
    snapshot_size INTEGER NOT NULL CHECK (snapshot_size >= 0),
    source TEXT NOT NULL,
    pins TEXT NOT NULL,
    UNIQUE (runbook_id, number)
);
CREATE INDEX IF NOT EXISTS runbook_version_latest ON runbook_version (runbook_id, number DESC)
    WHERE number IS NOT NULL;
