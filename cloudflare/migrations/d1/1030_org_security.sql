ALTER TABLE organization ADD COLUMN security TEXT NOT NULL
    DEFAULT '{"require_mfa":"off","allow_passkeys":true,"allow_authenticator":true,"fresh_minutes":10}';

CREATE TABLE org_security_audit (
    id INTEGER PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organization(org_id),
    actor TEXT NOT NULL,
    policy TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL
);
CREATE INDEX org_security_audit_org_idx ON org_security_audit (org_id, id);
