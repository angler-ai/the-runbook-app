-- B7a: execution credential families and hash-only token rows. Bodies are
-- the exact core records; hash and lifecycle columns serve lookups.
CREATE TABLE runbook.credential_family (
    family_id text PRIMARY KEY,
    execution_id text NOT NULL
        REFERENCES runbook.execution_owner ON DELETE CASCADE,
    org_id text NOT NULL,
    project_id text NOT NULL,
    issuer_token_id text NOT NULL,
    body text NOT NULL,
    idempotency_key text NOT NULL,
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    created_at_ms bigint NOT NULL,
    revoked_at_ms bigint,
    UNIQUE (org_id, project_id, execution_id, idempotency_key)
);
CREATE TABLE runbook.execution_credential (
    token_id text PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('execution', 'refresh')),
    hash bytea NOT NULL UNIQUE CHECK (octet_length(hash) = 32),
    family_id text NOT NULL REFERENCES runbook.credential_family ON DELETE CASCADE,
    execution_id text NOT NULL
        REFERENCES runbook.execution_owner ON DELETE CASCADE,
    org_id text NOT NULL,
    project_id text NOT NULL,
    generation bigint NOT NULL CHECK (generation > 0),
    issued_at_ms bigint NOT NULL,
    expires_at_ms bigint NOT NULL,
    consumed_at_ms bigint,
    revoked_at_ms bigint,
    body text NOT NULL
);
CREATE INDEX execution_credential_family
    ON runbook.execution_credential (family_id, generation);
