-- C1 follow-ups: a project-level outbox for webhook events with no
-- execution (runbook.published), written in the publishing transaction and
-- removed once fanned out; indexes for the 90-day delivery-log purge.
CREATE TABLE runbook.project_event_outbox (
    org_id text NOT NULL,
    project_id text NOT NULL,
    event_id text NOT NULL,
    created_at_ms bigint NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (org_id, project_id, event_id)
);
CREATE INDEX project_event_outbox_order
    ON runbook.project_event_outbox (org_id, project_id, created_at_ms, event_id);
CREATE INDEX webhook_delivery_finished_age
    ON runbook.webhook_delivery (org_id, project_id, created_at_ms)
    WHERE state <> 'pending';
CREATE INDEX webhook_attempt_age ON runbook.webhook_attempt (at_ms);
