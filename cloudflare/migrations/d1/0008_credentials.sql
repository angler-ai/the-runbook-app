-- D1: 0008_credentials.sql
-- Organization credentials (SHA-256 hashes only).

CREATE TABLE IF NOT EXISTS credential (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    secret_hash BLOB NOT NULL UNIQUE CHECK (length(secret_hash) = 32),
    payload TEXT NOT NULL,
    revoked_at INTEGER,
    last_used_at INTEGER
);
CREATE INDEX IF NOT EXISTS credential_org ON credential (org_id, id);
