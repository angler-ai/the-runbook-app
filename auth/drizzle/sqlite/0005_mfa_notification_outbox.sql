ALTER TABLE `auth_mfa_audit` ADD `notification` text;--> statement-breakpoint
ALTER TABLE `auth_mfa_audit` ADD `notify_attempted_at` integer;--> statement-breakpoint
ALTER TABLE `auth_mfa_audit` ADD `notified_at` integer;
--> statement-breakpoint
CREATE INDEX auth_mfa_notification_pending ON auth_mfa_audit (occurred_at)
WHERE notification IS NOT NULL AND notified_at IS NULL;
--> statement-breakpoint
-- expand-only: Only new MFA setup challenges are expired and an outbox row is appended; legacy verification identifiers and passkey fields are untouched.
CREATE TRIGGER auth_audit_passkey_enrollment AFTER INSERT ON auth_passkey
BEGIN
    UPDATE auth_verification SET expires_at = 0 WHERE identifier IN (
        SELECT 'totp-setup:' || id FROM auth_session WHERE user_id = NEW.user_id
    );
    INSERT INTO auth_mfa_audit (id, user_id, action, actor, occurred_at, notification)
    VALUES ('passkey-enroll:' || NEW.id, NEW.user_id, 'mfa.passkey_enrolled',
            NEW.user_id, COALESCE(NEW.created_at, CAST(unixepoch('subsecond') * 1000 AS integer)), 'enrolled');
END;
