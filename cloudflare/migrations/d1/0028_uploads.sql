-- D1: 0028_uploads.sql
-- Evidence upload metadata slots. File bytes live in R2 (BlobKey::Upload).

CREATE TABLE IF NOT EXISTS evidence (
    evidence_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    execution_id TEXT NOT NULL REFERENCES execution_owner (execution_id) ON DELETE CASCADE,
    step_run_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'finalized', 'rejected')),
    is_image INTEGER NOT NULL,
    declared_size INTEGER NOT NULL CHECK (declared_size >= 0),
    created_at_ms INTEGER NOT NULL,
    body TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS evidence_run
    ON evidence (execution_id, created_at_ms, evidence_id);
CREATE INDEX IF NOT EXISTS evidence_step_run
    ON evidence (org_id, project_id, step_run_id) WHERE state <> 'rejected';
CREATE INDEX IF NOT EXISTS evidence_pending
    ON evidence (org_id, project_id, created_at_ms, evidence_id)
    WHERE state = 'pending';
CREATE INDEX IF NOT EXISTS evidence_org_storage
    ON evidence (org_id) WHERE state <> 'rejected';
