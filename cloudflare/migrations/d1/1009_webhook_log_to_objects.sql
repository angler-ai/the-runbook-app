-- D1: 1009_webhook_log_to_objects.sql (CF11)
-- Each webhook subscription's delivery log (deliveries and attempts, with
-- their retry state) now lives in its own WebhookDO
-- (cloudflare/migrations/webhookdo/0001_delivery_log.sql), the user's
-- decision superseding D56's placement. Subscriptions, event records and
-- replay rows stay here. The local edition has no deployed data to carry.
DROP INDEX IF EXISTS webhook_attempt_age;
DROP TABLE IF EXISTS webhook_attempt;
DROP INDEX IF EXISTS webhook_delivery_finished_age;
DROP INDEX IF EXISTS webhook_delivery_log;
DROP INDEX IF EXISTS webhook_delivery_due;
DROP INDEX IF EXISTS webhook_delivery_first;
DROP TABLE IF EXISTS webhook_delivery;
