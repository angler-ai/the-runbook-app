CREATE TABLE runner_vault_identity_history (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    install_id TEXT NOT NULL REFERENCES runner_installs(id),
    public_key TEXT NOT NULL,
    changed_by TEXT,
    changed_at_ms INTEGER NOT NULL
);
CREATE INDEX runner_vault_identity_install ON runner_vault_identity_history (install_id, sequence DESC);
