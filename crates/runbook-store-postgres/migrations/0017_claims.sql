-- B6a: claim lease columns for due derivation and fenced heartbeats, plus
-- per-claim log lines. The exact ClaimWrite JSON body stays authoritative.
ALTER TABLE runbook.execution_claim
    ADD COLUMN lease_deadline_at_ms bigint,
    ADD COLUMN released boolean NOT NULL DEFAULT false;
ALTER TABLE runbook.work_lease DROP CONSTRAINT work_lease_kind_check;
ALTER TABLE runbook.work_lease
    ADD CONSTRAINT work_lease_kind_check
    CHECK (kind IN ('timer', 'notification', 'activation', 'claim'));
CREATE INDEX execution_claim_lease ON runbook.execution_claim (lease_deadline_at_ms)
    WHERE NOT released AND lease_deadline_at_ms IS NOT NULL;
CREATE TABLE runbook.claim_log (
    execution_id text NOT NULL
        REFERENCES runbook.execution_owner ON DELETE CASCADE,
    step_run_id text NOT NULL,
    seq bigint NOT NULL CHECK (seq > 0),
    line text NOT NULL,
    PRIMARY KEY (execution_id, step_run_id, seq)
);
