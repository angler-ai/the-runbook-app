-- B2.3: work leases for due timers and notification delivery. Due work is
-- derived from execution_timer and execution_outbox; this table owns only
-- the lease generation, deadline and acknowledgement that fence workers.
CREATE TABLE runbook.work_lease (
    org_id text NOT NULL,
    project_id text NOT NULL,
    execution_id text NOT NULL REFERENCES runbook.execution_owner ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('timer', 'notification')),
    -- Timer key, or notification event_id.
    work_key text NOT NULL,
    -- Timer generation for timers; 0 for notifications.
    work_generation bigint NOT NULL CHECK (work_generation >= 0),
    generation bigint NOT NULL CHECK (generation > 0),
    deadline_at_ms bigint NOT NULL,
    acknowledged boolean NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (org_id, project_id, execution_id, kind, work_key, work_generation)
);
