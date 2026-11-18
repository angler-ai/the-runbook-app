-- D1: 0021_webhooks.sql
-- Webhook subscriptions, recorded events, deliveries, attempts, and replay idempotency.

CREATE TABLE IF NOT EXISTS webhook_subscription (
    subscription_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    body TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS webhook_subscription_tenant
    ON webhook_subscription (org_id, project_id, created_at_ms);

CREATE TABLE IF NOT EXISTS webhook_event (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    execution_id TEXT,
    created_at_ms INTEGER NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (org_id, project_id, event_id)
);
CREATE INDEX IF NOT EXISTS webhook_event_execution
    ON webhook_event (org_id, project_id, execution_id)
    WHERE execution_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS webhook_delivery (
    delivery_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    subscription_id TEXT NOT NULL REFERENCES webhook_subscription (subscription_id) ON DELETE CASCADE,
    event_id TEXT NOT NULL,
    redelivery_of TEXT,
    state TEXT NOT NULL CHECK (state IN ('pending', 'delivered', 'failed')),
    next_attempt_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    generation INTEGER NOT NULL DEFAULT 0,
    lease_deadline_at_ms INTEGER,
    body TEXT NOT NULL,
    FOREIGN KEY (org_id, project_id, event_id) REFERENCES webhook_event (org_id, project_id, event_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS webhook_delivery_first
    ON webhook_delivery (org_id, project_id, event_id, subscription_id)
    WHERE redelivery_of IS NULL;
CREATE INDEX IF NOT EXISTS webhook_delivery_due
    ON webhook_delivery (org_id, project_id, next_attempt_at_ms)
    WHERE state = 'pending';
CREATE INDEX IF NOT EXISTS webhook_delivery_log
    ON webhook_delivery (subscription_id, created_at_ms DESC, delivery_id DESC);

CREATE TABLE IF NOT EXISTS webhook_attempt (
    delivery_id TEXT NOT NULL REFERENCES webhook_delivery (delivery_id) ON DELETE CASCADE,
    number INTEGER NOT NULL CHECK (number > 0),
    at_ms INTEGER NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (delivery_id, number)
);

CREATE TABLE IF NOT EXISTS webhook_replay (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    replay_key TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (org_id, project_id, replay_key)
);
