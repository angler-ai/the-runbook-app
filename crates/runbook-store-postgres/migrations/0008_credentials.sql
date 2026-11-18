-- Only SHA-256 hashes are stored. Payload contains typed credential metadata,
-- never the secret. Mutable timestamps have authoritative columns for atomic updates.
CREATE TABLE runbook.credential (
    id text PRIMARY KEY,
    org_id text NOT NULL,
    secret_hash bytea NOT NULL UNIQUE CHECK (octet_length(secret_hash) = 32),
    payload text NOT NULL,
    revoked_at bigint,
    last_used_at bigint
);
CREATE INDEX credential_org ON runbook.credential (org_id, id);
