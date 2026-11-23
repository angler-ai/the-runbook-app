ALTER TABLE credentials ADD COLUMN oauth_status jsonb;
ALTER TABLE credentials DROP CONSTRAINT credentials_status_check;
ALTER TABLE credentials ADD CONSTRAINT credentials_status_check
  CHECK (status IN ('needs_secret', 'active', 'revoked', 'reconnect_required'));
GRANT UPDATE (oauth_status) ON credentials TO rb_vault;

CREATE TABLE credential_oauth (
  credential_id text PRIMARY KEY REFERENCES credentials(id) ON DELETE CASCADE,
  generation integer NOT NULL DEFAULT 0,
  candidate jsonb,
  candidate_hash bytea,
  registration_operation text,
  client_secret_expires_at timestamptz,
  connection_hash bytea,
  connection_config jsonb,
  connection_groups jsonb,
  pending_state bytea,
  pending_user text,
  pending_session text,
  pending_nonce bytea,
  pending_hash bytea,
  pending_version integer,
  pending_expires_at timestamptz,
  pending_claim text,
  access_version integer,
  refresh_version integer,
  access_issued_at timestamptz,
  access_expires_at timestamptz,
  grant_expires_at timestamptz,
  refreshed_at timestamptz,
  refresh_operation text,
  refresh_after timestamptz,
  warned_at timestamptz
);
GRANT SELECT, INSERT, UPDATE, DELETE ON credential_oauth TO rb_vault;

CREATE FUNCTION oauth_config_vault_only() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
  IF NEW.type = 'mcp-oauth' AND NOT pg_has_role(current_user, 'rb_vault', 'MEMBER')
     AND NOT (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) THEN
    RAISE insufficient_privilege USING MESSAGE = 'OAuth credentials are managed by the vault';
  END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER oauth_config_vault_only BEFORE INSERT OR UPDATE OF config, binding, binding_hash, groups ON credentials
  FOR EACH ROW EXECUTE FUNCTION oauth_config_vault_only();

ALTER TABLE budget_event_outbox DROP CONSTRAINT budget_event_outbox_kind_check;
ALTER TABLE budget_event_outbox ADD CONSTRAINT budget_event_outbox_kind_check
  CHECK (kind IN ('runner.budget_threshold', 'runner.probe_succeeded', 'runner.decision_cost', 'runner.task_cost',
    'credential.reconnect_required', 'credential.oauth_expiring'));
GRANT INSERT (id, group_id, runner_id, kind, data) ON budget_event_outbox TO rb_vault;

GRANT SELECT (id, url, transport, credential_id, deleted_at) ON mcp_servers TO rb_vault;
