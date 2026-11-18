-- D1: 0016_owner_recovery_audit.sql
-- Local recovery audit trail for organization owner changes.

CREATE TABLE IF NOT EXISTS owner_recovery_audit (
    org_id TEXT NOT NULL REFERENCES organization (org_id),
    revision INTEGER NOT NULL CHECK (revision > 0),
    user_id TEXT NOT NULL REFERENCES user_identity (user_id),
    from_role TEXT NOT NULL CHECK (from_role IN ('member', 'admin')),
    to_role TEXT NOT NULL CHECK (to_role = 'owner'),
    actor TEXT NOT NULL CHECK (actor = 'local_cli'),
    database_role TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    PRIMARY KEY (org_id, revision)
);
