-- D1-only (CF5): the stored versions of each day's archive file, as the
-- Postgres `blob` rows of archive keys are. The bytes are R2's
-- (`archives/{runbook}/{date}/{digest}.zip`); a version row is written
-- before its object (`stored = 0`) and marked stored after, so recording a
-- file can require a stored version inside its D1 batch, and a sweep finds
-- every object a losing or interrupted write left (by `seq`, the write
-- order, `created_at_ms`, the grace period, and `named`).
CREATE TABLE IF NOT EXISTS archive_version (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    runbook_id TEXT NOT NULL,
    day INTEGER NOT NULL,
    digest BLOB NOT NULL CHECK (length(digest) = 32),
    seq INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    stored INTEGER NOT NULL DEFAULT 0 CHECK (stored IN (0, 1)),
    -- The version the day's index row names (kept by record_archive).
    named INTEGER NOT NULL DEFAULT 0 CHECK (named IN (0, 1)),
    PRIMARY KEY (org_id, project_id, runbook_id, day, digest)
);
CREATE INDEX IF NOT EXISTS archive_version_created
    ON archive_version (org_id, project_id, created_at_ms);
