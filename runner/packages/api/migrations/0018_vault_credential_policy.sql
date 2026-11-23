-- The vault owns label, revocation, deletion and secret activation. The API
-- can edit configuration. A changed destination requires resealing.
ALTER TABLE credentials ADD COLUMN label_initialized boolean NOT NULL DEFAULT true;
ALTER TABLE credentials ALTER COLUMN label_initialized SET DEFAULT false;
ALTER TABLE credentials ALTER COLUMN label DROP DEFAULT;

CREATE FUNCTION credential_config_state() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, public, pg_temp
AS $$
BEGIN
  IF TG_OP = 'INSERT' THEN
    NEW.label := COALESCE(NEW.label, CASE WHEN NEW.type = 'model-provider' THEN 'read' ELSE 'destructive' END);
  ELSIF OLD.binding_hash IS DISTINCT FROM NEW.binding_hash AND OLD.status <> 'revoked' THEN
    NEW.status := 'needs_secret';
    SELECT COALESCE(jsonb_object_agg(key, value - 'sealed_for_binding'), '{}'::jsonb)
      INTO NEW.secret_meta FROM jsonb_each(OLD.secret_meta);
  END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER credential_config_state BEFORE INSERT OR UPDATE OF binding_hash ON credentials
FOR EACH ROW EXECUTE FUNCTION credential_config_state();

REVOKE INSERT, UPDATE, DELETE ON credentials FROM rb_api;
GRANT INSERT (id, org_id, scope, project_id, name, type, config, binding, binding_hash, sharing, groups, delivery, created_by) ON credentials TO rb_api;
GRANT UPDATE (name, config, binding, binding_hash, sharing, groups, delivery, updated_at) ON credentials TO rb_api;
GRANT UPDATE (label, label_initialized, revoked_at, deleted_at) ON credentials TO rb_vault;
