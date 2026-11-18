-- Local-only D29 recovery. The directory revision is the transaction's audit identity.
CREATE TABLE runbook.owner_recovery_audit (
    org_id text NOT NULL REFERENCES runbook.organization,
    revision bigint NOT NULL CHECK (revision > 0),
    user_id text NOT NULL REFERENCES runbook.user_identity,
    from_role text NOT NULL CHECK (from_role IN ('member', 'admin')),
    to_role text NOT NULL CHECK (to_role = 'owner'),
    actor text NOT NULL CHECK (actor = 'local_cli'),
    database_role text NOT NULL,
    occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (org_id, revision)
);
