-- D1: 0026_memory.sql
-- Curated runbook memory entries, revision log, and FTS5 search index.

CREATE TABLE IF NOT EXISTS memory_entry (
    memory_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    scope_key TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('proposed', 'active', 'archived')),
    step_id TEXT,
    tags TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    expires_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    source_execution_id TEXT,
    proposal_sequence INTEGER,
    body TEXT NOT NULL,
    search_tsv FTS5 NOT NULL,
    CHECK ((source_execution_id IS NULL) = (proposal_sequence IS NULL))
);
CREATE INDEX IF NOT EXISTS memory_entry_quota
    ON memory_entry (org_id, project_id, scope_key, status);
CREATE INDEX IF NOT EXISTS memory_entry_recent
    ON memory_entry (org_id, project_id, updated_at_ms DESC, memory_id DESC);
CREATE INDEX IF NOT EXISTS memory_entry_step
    ON memory_entry (org_id, project_id, step_id) WHERE status = 'active';
CREATE INDEX IF NOT EXISTS memory_entry_proposal
    ON memory_entry (source_execution_id, proposal_sequence)
    WHERE source_execution_id IS NOT NULL;

CREATE VIRTUAL TABLE IF NOT EXISTS memory_entry_fts USING fts5(
    memory_id UNINDEXED,
    body,
    content='memory_entry',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS memory_entry_ai AFTER INSERT ON memory_entry BEGIN
    INSERT INTO memory_entry_fts (rowid, memory_id, body)
    VALUES (new.rowid, new.memory_id, new.body);
END;

CREATE TRIGGER IF NOT EXISTS memory_entry_ad AFTER DELETE ON memory_entry BEGIN
    INSERT INTO memory_entry_fts (memory_entry_fts, rowid, memory_id, body)
    VALUES ('delete', old.rowid, old.memory_id, old.body);
END;

CREATE TRIGGER IF NOT EXISTS memory_entry_au AFTER UPDATE ON memory_entry BEGIN
    INSERT INTO memory_entry_fts (memory_entry_fts, rowid, memory_id, body)
    VALUES ('delete', old.rowid, old.memory_id, old.body);
    INSERT INTO memory_entry_fts (rowid, memory_id, body)
    VALUES (new.rowid, new.memory_id, new.body);
END;

CREATE TABLE IF NOT EXISTS memory_revision (
    memory_id TEXT NOT NULL REFERENCES memory_entry (memory_id),
    revision INTEGER NOT NULL CHECK (revision > 0),
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    action TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (memory_id, revision)
);
CREATE INDEX IF NOT EXISTS memory_revision_tenant ON memory_revision (org_id, project_id);
