-- D1: 0012_run_search.sql
-- Run summary search projection columns and FTS5 search index.

ALTER TABLE run_summary ADD COLUMN title TEXT NOT NULL DEFAULT '';
ALTER TABLE run_summary ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
ALTER TABLE run_summary ADD COLUMN fields TEXT NOT NULL DEFAULT '{}';
ALTER TABLE run_summary ADD COLUMN created_by TEXT NOT NULL DEFAULT '';
ALTER TABLE run_summary ADD COLUMN created_at_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE run_summary ADD COLUMN search_tsv FTS5;

CREATE INDEX IF NOT EXISTS run_summary_created
    ON run_summary (created_at_ms DESC, execution_id DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS run_summary_fts USING fts5(
    execution_id UNINDEXED,
    title,
    body,
    content='run_summary',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS run_summary_ai AFTER INSERT ON run_summary BEGIN
    INSERT INTO run_summary_fts (rowid, execution_id, title, body)
    VALUES (new.rowid, new.execution_id, new.title, new.body);
END;

CREATE TRIGGER IF NOT EXISTS run_summary_ad AFTER DELETE ON run_summary BEGIN
    INSERT INTO run_summary_fts (run_summary_fts, rowid, execution_id, title, body)
    VALUES ('delete', old.rowid, old.execution_id, old.title, old.body);
END;

CREATE TRIGGER IF NOT EXISTS run_summary_au AFTER UPDATE ON run_summary BEGIN
    INSERT INTO run_summary_fts (run_summary_fts, rowid, execution_id, title, body)
    VALUES ('delete', old.rowid, old.execution_id, old.title, old.body);
    INSERT INTO run_summary_fts (rowid, execution_id, title, body)
    VALUES (new.rowid, new.execution_id, new.title, new.body);
END;
