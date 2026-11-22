-- expand-only: Only changes to backup codes on verified MFA factors append an audit row; the previous service has no writes to the factor table.
CREATE TRIGGER auth_audit_backup_codes AFTER UPDATE OF backup_codes ON auth_two_factor
WHEN OLD.verified IS TRUE AND NEW.verified IS TRUE AND OLD.backup_codes <> NEW.backup_codes
BEGIN
    INSERT INTO auth_mfa_audit (id, user_id, action, actor, occurred_at, notification)
    VALUES (lower(hex(randomblob(16))), NEW.user_id,
        CASE WHEN json_array_length(NEW.backup_codes) < json_array_length(OLD.backup_codes)
            THEN 'mfa.backup_code_used' ELSE 'mfa.backup_codes_regenerated' END,
        NEW.user_id, CAST(unixepoch('subsecond') * 1000 AS integer),
        CASE WHEN json_array_length(NEW.backup_codes) < json_array_length(OLD.backup_codes)
            THEN 'used' ELSE 'regenerated' END);
END;
