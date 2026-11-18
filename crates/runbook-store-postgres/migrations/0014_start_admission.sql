-- B5a: admission time anchors the execution deadline (D24); activation is
-- a work kind; start records and start replay results land atomically
-- with execution initialization and admission.
ALTER TABLE runbook.admission ADD COLUMN admitted_at_ms bigint;
-- D25: lowering a pool limit preempts nobody, so active may exceed it.
ALTER TABLE runbook.admission_pool DROP CONSTRAINT admission_pool_check;
ALTER TABLE runbook.admission_pool
    ADD CONSTRAINT admission_pool_active_nonnegative CHECK (active >= 0);
ALTER TABLE runbook.work_lease DROP CONSTRAINT work_lease_kind_check;
ALTER TABLE runbook.work_lease
    ADD CONSTRAINT work_lease_kind_check
    CHECK (kind IN ('timer', 'notification', 'activation'));
CREATE TABLE runbook.execution_start (
    execution_id text PRIMARY KEY
        REFERENCES runbook.execution_owner ON DELETE CASCADE,
    org_id text NOT NULL,
    project_id text NOT NULL,
    body text NOT NULL
);
CREATE TABLE runbook.start_replay (
    org_id text NOT NULL,
    project_id text NOT NULL,
    idempotency_key text NOT NULL,
    principal text NOT NULL,
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    execution_id text NOT NULL
        REFERENCES runbook.execution_owner ON DELETE CASCADE,
    outcome text NOT NULL,
    PRIMARY KEY (org_id, project_id, idempotency_key)
);
