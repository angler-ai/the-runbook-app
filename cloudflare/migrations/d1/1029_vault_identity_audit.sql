CREATE TABLE runner_vault_identity_audit (
    sequence INTEGER NOT NULL PRIMARY KEY,
    install_id TEXT NOT NULL,
    org_id TEXT NOT NULL,
    action TEXT NOT NULL,
    actor TEXT NOT NULL,
    public_key TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL
);
CREATE INDEX runner_vault_identity_audit_install ON runner_vault_identity_audit (install_id, sequence);
