-- D1: 1013_webhook_subscription_deleted.sql (gate round 2)
-- A deleted subscription's marker, written in the same batch as its
-- deletion. A subscription's WebhookDO purges and retires itself only when
-- this marker names it: positive evidence, never a read that merely does
-- not find the subscription (a briefly wrong shard map, a lagging read).
CREATE TABLE IF NOT EXISTS webhook_subscription_deleted (
    subscription_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    deleted_at_ms INTEGER NOT NULL
);
