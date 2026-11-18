-- C1: webhook subscriptions, recorded events, deliveries and attempts.
-- Bodies are the exact core records (the subscription body holds only the
-- sealed secret); scalar columns serve tenant filters, claims and limits.
CREATE TABLE runbook.webhook_subscription (
    subscription_id text PRIMARY KEY,
    org_id text NOT NULL,
    project_id text NOT NULL,
    created_at_ms bigint NOT NULL,
    body text NOT NULL
);
CREATE INDEX webhook_subscription_tenant
    ON runbook.webhook_subscription (org_id, project_id, created_at_ms);
CREATE TABLE runbook.webhook_event (
    org_id text NOT NULL,
    project_id text NOT NULL,
    event_id text NOT NULL,
    execution_id text,
    created_at_ms bigint NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (org_id, project_id, event_id)
);
CREATE INDEX webhook_event_execution
    ON runbook.webhook_event (org_id, project_id, execution_id)
    WHERE execution_id IS NOT NULL;
CREATE TABLE runbook.webhook_delivery (
    delivery_id text PRIMARY KEY,
    org_id text NOT NULL,
    project_id text NOT NULL,
    subscription_id text NOT NULL
        REFERENCES runbook.webhook_subscription ON DELETE CASCADE,
    event_id text NOT NULL,
    redelivery_of text,
    state text NOT NULL CHECK (state IN ('pending', 'delivered', 'failed')),
    next_attempt_at_ms bigint,
    created_at_ms bigint NOT NULL,
    generation bigint NOT NULL DEFAULT 0,
    lease_deadline_at_ms bigint,
    body text NOT NULL,
    FOREIGN KEY (org_id, project_id, event_id)
        REFERENCES runbook.webhook_event (org_id, project_id, event_id)
);
-- One first delivery per (event, subscription); redeliveries are extra rows.
CREATE UNIQUE INDEX webhook_delivery_first
    ON runbook.webhook_delivery (org_id, project_id, event_id, subscription_id)
    WHERE redelivery_of IS NULL;
CREATE INDEX webhook_delivery_due
    ON runbook.webhook_delivery (org_id, project_id, next_attempt_at_ms)
    WHERE state = 'pending';
CREATE INDEX webhook_delivery_log
    ON runbook.webhook_delivery (subscription_id, created_at_ms DESC, delivery_id DESC);
CREATE TABLE runbook.webhook_attempt (
    delivery_id text NOT NULL
        REFERENCES runbook.webhook_delivery ON DELETE CASCADE,
    number integer NOT NULL CHECK (number > 0),
    at_ms bigint NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (delivery_id, number)
);
-- D20 replay rows for webhook POSTs, retained 24 hours.
CREATE TABLE runbook.webhook_replay (
    org_id text NOT NULL,
    project_id text NOT NULL,
    replay_key text NOT NULL,
    created_at_ms bigint NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (org_id, project_id, replay_key)
);
