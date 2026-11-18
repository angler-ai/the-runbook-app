-- B2.2a: execution-owned rows and the derived project catalog.
-- Authoritative payloads are exact serde JSON in text; jsonb would reorder keys
-- and normalize numbers. Scalar columns beside them exist only for keys and indexes.

-- One owner per execution. Admission may claim it before the execution head exists.
CREATE TABLE runbook.execution_owner (
    execution_id text PRIMARY KEY,
    org_id text NOT NULL,
    project_id text NOT NULL
);
CREATE INDEX execution_owner_scope ON runbook.execution_owner (org_id, project_id);

CREATE TABLE runbook.execution (
    execution_id text PRIMARY KEY REFERENCES runbook.execution_owner ON DELETE CASCADE,
    revision bigint NOT NULL CHECK (revision >= 0),
    runbook_id text NOT NULL,
    status text NOT NULL,
    completed_at_ms bigint,
    projection text NOT NULL
);
CREATE INDEX execution_completed ON runbook.execution (runbook_id, completed_at_ms)
    WHERE completed_at_ms IS NOT NULL;

CREATE TABLE runbook.execution_event (
    execution_id text NOT NULL REFERENCES runbook.execution ON DELETE CASCADE,
    seq bigint NOT NULL CHECK (seq > 0),
    body text NOT NULL,
    PRIMARY KEY (execution_id, seq)
);
CREATE TABLE runbook.execution_timer (
    execution_id text NOT NULL REFERENCES runbook.execution ON DELETE CASCADE,
    key text NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (execution_id, key)
);
-- Latest incarnation per key, kept after deletion so a generation is never reused.
CREATE TABLE runbook.execution_timer_history (
    execution_id text NOT NULL REFERENCES runbook.execution ON DELETE CASCADE,
    key text NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (execution_id, key)
);
CREATE TABLE runbook.execution_outbox (
    execution_id text NOT NULL REFERENCES runbook.execution ON DELETE CASCADE,
    event_id text NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (execution_id, event_id)
);
CREATE TABLE runbook.execution_idempotency (
    execution_id text NOT NULL REFERENCES runbook.execution ON DELETE CASCADE,
    key text NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (execution_id, key)
);
CREATE TABLE runbook.execution_claim (
    execution_id text NOT NULL REFERENCES runbook.execution ON DELETE CASCADE,
    step_run_id text NOT NULL,
    generation bigint NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (execution_id, step_run_id)
);
CREATE TABLE runbook.execution_derived_outbox (
    execution_id text NOT NULL REFERENCES runbook.execution ON DELETE CASCADE,
    sequence bigint NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (execution_id, sequence)
);

-- Catalog. Drafts are project-scoped; run rows hang off the execution owner.
CREATE TABLE runbook.draft (
    org_id text NOT NULL,
    project_id text NOT NULL,
    key text NOT NULL CHECK (key <> ''),
    revision bigint NOT NULL CHECK (revision > 0),
    source text NOT NULL,
    PRIMARY KEY (org_id, project_id, key)
);
CREATE TABLE runbook.run_summary (
    execution_id text PRIMARY KEY REFERENCES runbook.execution_owner ON DELETE CASCADE,
    runbook_id text NOT NULL,
    sequence bigint NOT NULL,
    status text NOT NULL,
    completed_at_ms bigint,
    body text NOT NULL
);
CREATE TABLE runbook.assignment (
    execution_id text NOT NULL REFERENCES runbook.execution_owner ON DELETE CASCADE,
    -- Preserves the order the engine supplied within one execution.
    ordinal integer NOT NULL,
    step_run_id text NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (execution_id, step_run_id)
);
CREATE TABLE runbook.admission_pool (
    org_id text NOT NULL,
    project_id text NOT NULL,
    key text NOT NULL CHECK (key <> ''),
    concurrency_limit integer NOT NULL CHECK (concurrency_limit > 0),
    active integer NOT NULL CHECK (active >= 0 AND active <= concurrency_limit),
    PRIMARY KEY (org_id, project_id, key)
);
CREATE SEQUENCE runbook.admission_queue_order;
CREATE TABLE runbook.admission (
    execution_id text PRIMARY KEY REFERENCES runbook.execution_owner ON DELETE CASCADE,
    org_id text NOT NULL,
    project_id text NOT NULL,
    key text NOT NULL,
    state text NOT NULL CHECK (state IN ('admitted', 'queued', 'released')),
    -- Set only while queued; FIFO order within the pool.
    queue_order bigint,
    request text NOT NULL,
    CHECK ((state = 'queued') = (queue_order IS NOT NULL)),
    FOREIGN KEY (org_id, project_id, key) REFERENCES runbook.admission_pool
);
CREATE INDEX admission_queue ON runbook.admission (org_id, project_id, key, queue_order)
    WHERE queue_order IS NOT NULL;
CREATE TABLE runbook.start_idempotency (
    org_id text NOT NULL,
    project_id text NOT NULL,
    idempotency_key text NOT NULL,
    execution_id text NOT NULL REFERENCES runbook.execution_owner ON DELETE CASCADE,
    request text NOT NULL,
    PRIMARY KEY (org_id, project_id, idempotency_key)
);
