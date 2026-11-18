-- D1: 0022_webhook_follow_ups.sql
-- Project-level outbox for standalone webhook events and delivery log purge indexes.

CREATE TABLE IF NOT EXISTS project_event_outbox (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (org_id, project_id, event_id)
);
CREATE INDEX IF NOT EXISTS project_event_outbox_order
    ON project_event_outbox (org_id, project_id, created_at_ms, event_id);
CREATE INDEX IF NOT EXISTS webhook_delivery_finished_age
    ON webhook_delivery (org_id, project_id, created_at_ms)
    WHERE state <> 'pending';
CREATE INDEX IF NOT EXISTS webhook_attempt_age ON webhook_attempt (at_ms);
