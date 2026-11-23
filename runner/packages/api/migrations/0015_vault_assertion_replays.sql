CREATE TABLE vault_assertion_replays (
  install_id text NOT NULL,
  jti text NOT NULL,
  expires_at timestamptz NOT NULL,
  PRIMARY KEY (install_id, jti)
);
CREATE INDEX vault_assertion_replays_expiry ON vault_assertion_replays (expires_at);
GRANT SELECT, INSERT, DELETE ON vault_assertion_replays TO rb_vault;
