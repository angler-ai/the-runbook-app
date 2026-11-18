-- ARC1/ARC2 (PRD §14 "Retention and daily archives"; D53–D54): the daily
-- archive index and the summary rows that outlive live detail.
--
-- Archive files are blobs (BlobKey::Archive, addressed by their digest)
-- owned by a runbook of the tenant: a third kind of blob row beside run
-- blobs and content-addressed shared blobs, so exactly one owner column is
-- set. Retention's live deletion also moves a run's upload rows to its
-- runbook (PRD §14: evidence follows the archive), so they survive the
-- run's owner row until the file expires.
ALTER TABLE runbook.blob ADD COLUMN runbook_id text
    REFERENCES runbook.runbook (runbook_id) ON DELETE CASCADE;
ALTER TABLE runbook.blob DROP CONSTRAINT blob_check;
ALTER TABLE runbook.blob ADD CONSTRAINT blob_one_owner
    CHECK (num_nonnulls(execution_id, hash, runbook_id) = 1);
CREATE INDEX blob_runbook ON runbook.blob (runbook_id)
    WHERE runbook_id IS NOT NULL;
-- When an archive version was put (store clock, microseconds): the store
-- deletes only versions older than the one the index names, or than an
-- hour, so another writer's in-flight version is never touched.
ALTER TABLE runbook.blob ADD COLUMN created_at_us bigint;

-- One row per (tenant, runbook, UTC day) file. `body` is the exact encoded
-- ArchiveRecord; `stale` is authoritative for the flag (stale marks touch
-- only the column, inside the transaction that made the file out of date).
CREATE TABLE runbook.archive (
    org_id text NOT NULL,
    project_id text NOT NULL,
    runbook_id text NOT NULL,
    day bigint NOT NULL,
    stale boolean NOT NULL,
    -- Every stale mark increments it; a record started before a mark stays
    -- stale, so a record never clears a concurrent mark.
    stale_marks bigint NOT NULL DEFAULT 0,
    body text NOT NULL,
    PRIMARY KEY (org_id, project_id, runbook_id, day)
);
CREATE INDEX archive_stale ON runbook.archive (org_id, project_id, day, runbook_id)
    WHERE stale;
CREATE INDEX archive_day ON runbook.archive (org_id, project_id, day, runbook_id);

-- The summary row of each archived run (ArchivedRun in `body`): where it
-- lives in its file. It is not a run-owned row: retention's deletion keeps
-- it; any other deletion removes it and marks the file stale. It outlives
-- its file (no foreign key): summaries may be kept longer than files
-- (`summary_days` vs `archive_days`), and only purge_summaries or a
-- single-run deletion drop it.
CREATE TABLE runbook.archive_run (
    execution_id text PRIMARY KEY,
    org_id text NOT NULL,
    project_id text NOT NULL,
    runbook_id text NOT NULL,
    day bigint NOT NULL,
    completed_at_ms bigint NOT NULL,
    body text NOT NULL
);
CREATE INDEX archive_run_file ON runbook.archive_run (org_id, project_id, runbook_id, day);
CREATE INDEX archive_run_completed
    ON runbook.archive_run (org_id, project_id, completed_at_ms, execution_id COLLATE "C");

-- A day whose write failed backs off so later days still progress: attempts
-- and the next attempt time. Cleared when the day's file is recorded or
-- dropped.
CREATE TABLE runbook.archive_attempt (
    org_id text NOT NULL,
    project_id text NOT NULL,
    runbook_id text NOT NULL,
    day bigint NOT NULL,
    attempts integer NOT NULL CHECK (attempts > 0),
    next_attempt_at_ms bigint NOT NULL,
    PRIMARY KEY (org_id, project_id, runbook_id, day)
);

-- Every run a file lists, until the file is deleted, whether or not it keeps
-- a summary row: stale marks and single-run deletions find the file through
-- it, even after the summary row was purged.
CREATE TABLE runbook.archive_member (
    execution_id text PRIMARY KEY,
    org_id text NOT NULL,
    project_id text NOT NULL,
    runbook_id text NOT NULL,
    day bigint NOT NULL
);
CREATE INDEX archive_member_file ON runbook.archive_member (org_id, project_id, runbook_id, day);

-- Evidence of runs retention deleted (PRD §14: the uploads follow the
-- archive): the bytes keep counting toward the organization's storage
-- until the file expires or a single-run deletion.
CREATE TABLE runbook.retained_evidence (
    evidence_id text PRIMARY KEY,
    org_id text NOT NULL,
    project_id text NOT NULL,
    execution_id text NOT NULL,
    counted_bytes bigint NOT NULL CHECK (counted_bytes >= 0)
);
CREATE INDEX retained_evidence_org ON runbook.retained_evidence (org_id);
CREATE INDEX retained_evidence_run ON runbook.retained_evidence (execution_id);
