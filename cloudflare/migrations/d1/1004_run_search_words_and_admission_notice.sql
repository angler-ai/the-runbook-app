-- D1-only (CF3).
--
-- 1. Index run summaries' words, not their JSON (as 1003 does for memory):
--    `search_tsv` holds the words of the title and tags, which is what run
--    search matches (Postgres: to_tsvector('simple', title || tags)).
DROP TRIGGER IF EXISTS run_summary_ai;
DROP TRIGGER IF EXISTS run_summary_ad;
DROP TRIGGER IF EXISTS run_summary_au;
DROP TABLE IF EXISTS run_summary_fts;

CREATE VIRTUAL TABLE IF NOT EXISTS run_summary_fts USING fts5(
    execution_id UNINDEXED,
    search_tsv,
    content='run_summary',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS run_summary_ai AFTER INSERT ON run_summary BEGIN
    INSERT INTO run_summary_fts (rowid, execution_id, search_tsv)
    VALUES (new.rowid, new.execution_id, new.search_tsv);
END;

CREATE TRIGGER IF NOT EXISTS run_summary_ad AFTER DELETE ON run_summary BEGIN
    INSERT INTO run_summary_fts (run_summary_fts, rowid, execution_id, search_tsv)
    VALUES ('delete', old.rowid, old.execution_id, old.search_tsv);
END;

CREATE TRIGGER IF NOT EXISTS run_summary_au AFTER UPDATE ON run_summary BEGIN
    INSERT INTO run_summary_fts (run_summary_fts, rowid, execution_id, search_tsv)
    VALUES ('delete', old.rowid, old.execution_id, old.search_tsv);
    INSERT INTO run_summary_fts (rowid, execution_id, search_tsv)
    VALUES (new.rowid, new.execution_id, new.search_tsv);
END;

INSERT INTO run_summary_fts (run_summary_fts) VALUES ('rebuild');

-- 2. Admission notices: a queued run promoted in D1 (a release, a raised
--    limit) must be told in its own ExecutionDO before it activates (D24).
--    The promotion writes this row in the same batch; the host delivers
--    `admit` to the object and deletes the row, and the minute cron
--    redelivers what is left, so a crash never strands a promoted run.
CREATE TABLE IF NOT EXISTS admission_notice (
    execution_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    admitted_at_ms INTEGER NOT NULL
);
