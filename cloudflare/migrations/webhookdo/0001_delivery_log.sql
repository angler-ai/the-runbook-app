-- WebhookDO 0001 (CF11): one webhook subscription's delivery log, moved out
-- of D1 (the user's decision, superseding D56's placement). The two
-- Postgres tables keep their columns exactly (the parity test compares
-- them); the object adds its owner and a copy of each event it delivers, so
-- an attempt or a retry needs no D1 read.
CREATE TABLE webhook_delivery (
  delivery_id TEXT PRIMARY KEY NOT NULL,
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  event_id TEXT NOT NULL,
  redelivery_of TEXT,
  state TEXT NOT NULL CHECK (state IN ('pending', 'delivered', 'failed')),
  next_attempt_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL,
  generation INTEGER NOT NULL DEFAULT 0,
  lease_deadline_at_ms INTEGER,
  body TEXT NOT NULL
);
-- One first delivery per event; redeliveries are extra rows.
CREATE UNIQUE INDEX webhook_delivery_first ON webhook_delivery (event_id)
  WHERE redelivery_of IS NULL;
CREATE INDEX webhook_delivery_due ON webhook_delivery (next_attempt_at_ms)
  WHERE state = 'pending';
CREATE INDEX webhook_delivery_log ON webhook_delivery (created_at_ms DESC, delivery_id DESC);
CREATE TABLE webhook_attempt (
  delivery_id TEXT NOT NULL REFERENCES webhook_delivery (delivery_id) ON DELETE CASCADE,
  number INTEGER NOT NULL CHECK (number > 0),
  at_ms INTEGER NOT NULL,
  body TEXT NOT NULL,
  PRIMARY KEY (delivery_id, number)
);
CREATE INDEX webhook_attempt_age ON webhook_attempt (at_ms);
-- The event each delivery carries (D1's webhook_event stays the tenant's
-- record of it); `human` marks events whose delivery states are a run's
-- notified/not-delivered verdict, kept past the log's retention.
CREATE TABLE delivery_event (
  event_id TEXT PRIMARY KEY NOT NULL,
  created_at_ms INTEGER NOT NULL,
  human INTEGER NOT NULL CHECK (human IN (0, 1)),
  body TEXT NOT NULL
);
-- The subscription this object holds, fixed by its first write.
CREATE TABLE owner (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL
);
