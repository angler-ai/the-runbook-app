CREATE TABLE runbook.runner_vault_identity_history (
    sequence BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    install_id TEXT NOT NULL REFERENCES runbook.runner_installs(id),
    public_key TEXT NOT NULL,
    changed_by TEXT,
    changed_at_ms BIGINT NOT NULL
);
CREATE INDEX runner_vault_identity_install ON runbook.runner_vault_identity_history (install_id, sequence DESC);
