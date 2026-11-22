CREATE TABLE assertion_replays (
  install_id text NOT NULL,
  jti text NOT NULL,
  expires_at timestamptz NOT NULL,
  PRIMARY KEY (install_id, jti)
);
CREATE INDEX assertion_replays_expiry ON assertion_replays (expires_at);
GRANT SELECT, INSERT, DELETE ON assertion_replays TO rb_api;
