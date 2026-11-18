-- D1: 0029_archives_retention.sql
-- Daily archive index and retained summary/evidence rows that outlive live detail.
-- Blob modifications belong to R2, so blob columns are omitted from D1.

CREATE TABLE IF NOT EXISTS archive (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    runbook_id TEXT NOT NULL,
    day INTEGER NOT NULL,
    stale INTEGER NOT NULL,
    stale_marks INTEGER NOT NULL DEFAULT 0,
    body TEXT NOT NULL,
    PRIMARY KEY (org_id, project_id, runbook_id, day)
);
CREATE INDEX IF NOT EXISTS archive_stale ON archive (org_id, project_id, day, runbook_id)
    WHERE stale;
CREATE INDEX IF NOT EXISTS archive_day ON archive (org_id, project_id, day, runbook_id);

CREATE TABLE IF NOT EXISTS archive_run (
    execution_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    runbook_id TEXT NOT NULL,
    day INTEGER NOT NULL,
    completed_at_ms INTEGER NOT NULL,
    body TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS archive_run_file ON archive_run (org_id, project_id, runbook_id, day);
CREATE INDEX IF NOT EXISTS archive_run_completed
    ON archive_run (org_id, project_id, completed_at_ms, execution_id);

CREATE TABLE IF NOT EXISTS archive_attempt (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    runbook_id TEXT NOT NULL,
    day INTEGER NOT NULL,
    attempts INTEGER NOT NULL CHECK (attempts > 0),
    next_attempt_at_ms INTEGER NOT NULL,
    PRIMARY KEY (org_id, project_id, runbook_id, day)
);

CREATE TABLE IF NOT EXISTS archive_member (
    execution_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    runbook_id TEXT NOT NULL,
    day INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS archive_member_file ON archive_member (org_id, project_id, runbook_id, day);

CREATE TABLE IF NOT EXISTS retained_evidence (
    evidence_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    counted_bytes INTEGER NOT NULL CHECK (counted_bytes >= 0)
);
CREATE INDEX IF NOT EXISTS retained_evidence_org ON retained_evidence (org_id);
CREATE INDEX IF NOT EXISTS retained_evidence_run ON retained_evidence (execution_id);
