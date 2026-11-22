-- expand-only: Only changes to backup codes on verified MFA factors append an audit row; the previous service has no writes to the factor table.
CREATE FUNCTION "auth"."audit_backup_codes"() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE action_name text;
DECLARE notice text;
BEGIN
    IF jsonb_array_length(NEW.backup_codes::jsonb) < jsonb_array_length(OLD.backup_codes::jsonb) THEN
        action_name := 'mfa.backup_code_used';
        notice := 'used';
    ELSE
        action_name := 'mfa.backup_codes_regenerated';
        notice := 'regenerated';
    END IF;
    INSERT INTO "auth"."mfa_audit" (id, user_id, action, actor, occurred_at, notification)
    VALUES (gen_random_uuid()::text, NEW.user_id, action_name, NEW.user_id, CURRENT_TIMESTAMP, notice);
    RETURN NEW;
END;
$$;
--> statement-breakpoint
-- expand-only: Only changes to backup codes on verified MFA factors append an audit row; the previous service has no writes to the factor table.
CREATE TRIGGER "audit_backup_codes" AFTER UPDATE OF backup_codes ON "auth"."two_factor"
FOR EACH ROW WHEN (OLD.verified IS TRUE AND NEW.verified IS TRUE AND OLD.backup_codes <> NEW.backup_codes)
EXECUTE FUNCTION "auth"."audit_backup_codes"();
