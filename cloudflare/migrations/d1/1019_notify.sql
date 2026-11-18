-- NOTIFY1: per-person email preferences and the durable email outbox
-- (the D1 mirror of Postgres 0035_notify.sql).
CREATE TABLE email_preference (
    user_id TEXT PRIMARY KEY,
    preference TEXT NOT NULL CHECK (preference IN ('all', 'approvals_only', 'daily_summary', 'none')),
    token_salt TEXT
);

CREATE TABLE email_delivery (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL,
    user_id TEXT,
    email TEXT NOT NULL,
    kind TEXT NOT NULL,
    execution_id TEXT,
    event_id TEXT NOT NULL,
    subject TEXT NOT NULL,
    text_body TEXT NOT NULL,
    html_body TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'digest', 'sent', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    sent_at_ms INTEGER,
    failure TEXT,
    generation INTEGER NOT NULL DEFAULT 0,
    lease_until_ms INTEGER,
    UNIQUE (event_id, user_id)
);

CREATE INDEX email_delivery_due
    ON email_delivery (next_attempt_at_ms, created_at_ms, id)
    WHERE state = 'pending';

CREATE INDEX email_delivery_digest
    ON email_delivery (created_at_ms, id)
    WHERE state = 'digest';
