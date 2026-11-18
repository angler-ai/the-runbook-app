-- NOTIFY1: per-person email preferences and the durable email outbox.
-- Two new tables, nothing else changes. Deliveries are claimed under a
-- generation-fenced lease and are unique per (event, person), so a
-- retried fan-out writes the same row once.
CREATE TABLE runbook.email_preference (
    user_id text PRIMARY KEY,
    preference text NOT NULL CHECK (preference IN ('all', 'approvals_only', 'daily_summary', 'none')),
    token_salt text
);

CREATE TABLE runbook.email_delivery (
    id text PRIMARY KEY,
    org_id text NOT NULL,
    user_id text,
    email text NOT NULL,
    kind text NOT NULL,
    execution_id text,
    event_id text NOT NULL,
    subject text NOT NULL,
    text_body text NOT NULL,
    html_body text NOT NULL,
    state text NOT NULL CHECK (state IN ('pending', 'digest', 'sent', 'failed')),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at_ms bigint NOT NULL,
    created_at_ms bigint NOT NULL,
    sent_at_ms bigint,
    failure text,
    generation bigint NOT NULL DEFAULT 0,
    lease_until_ms bigint,
    UNIQUE (event_id, user_id)
);

CREATE INDEX email_delivery_due
    ON runbook.email_delivery (next_attempt_at_ms, created_at_ms, id)
    WHERE state = 'pending';

CREATE INDEX email_delivery_digest
    ON runbook.email_delivery (created_at_ms, id)
    WHERE state = 'digest';
