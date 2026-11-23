CREATE TABLE runbook.runner_vault_identity_audit (
    sequence BIGINT PRIMARY KEY,
    install_id TEXT NOT NULL,
    org_id TEXT NOT NULL,
    action TEXT NOT NULL,
    actor TEXT NOT NULL,
    public_key TEXT NOT NULL,
    occurred_at_ms BIGINT NOT NULL
);
CREATE INDEX runner_vault_identity_audit_install ON runbook.runner_vault_identity_audit (install_id, sequence);
