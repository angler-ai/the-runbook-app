-- UP1 (PRD §14 "Uploads and images"; D49–D51): evidence upload slots.
-- Every row carries org_id and project_id and belongs to its run (deleted
-- with it). `body` is the exact encoded record; the other columns index it.
-- The bytes live in runbook.blob under the run's execution_id with a NULL
-- name (BlobKey::Upload); the key column holds the exact typed key.
CREATE TABLE runbook.evidence (
    evidence_id text PRIMARY KEY,
    org_id text NOT NULL,
    project_id text NOT NULL,
    execution_id text NOT NULL REFERENCES runbook.execution_owner ON DELETE CASCADE,
    step_run_id text NOT NULL,
    state text NOT NULL CHECK (state IN ('pending', 'finalized', 'rejected')),
    is_image boolean NOT NULL,
    declared_size bigint NOT NULL CHECK (declared_size >= 0),
    created_at_ms bigint NOT NULL,
    body text NOT NULL
);
-- Leads with the run: the cascade from execution_owner and the per-run
-- listing both use it (F36).
CREATE INDEX evidence_run
    ON runbook.evidence (execution_id, created_at_ms, evidence_id COLLATE "C");
CREATE INDEX evidence_step_run
    ON runbook.evidence (org_id, project_id, step_run_id) WHERE state <> 'rejected';
CREATE INDEX evidence_pending
    ON runbook.evidence (org_id, project_id, created_at_ms, evidence_id COLLATE "C")
    WHERE state = 'pending';
CREATE INDEX evidence_org_storage
    ON runbook.evidence (org_id) WHERE state <> 'rejected';
