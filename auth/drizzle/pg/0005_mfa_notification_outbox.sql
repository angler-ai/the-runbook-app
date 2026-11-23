ALTER TABLE "auth"."mfa_audit" ADD COLUMN "notification" text;--> statement-breakpoint
ALTER TABLE "auth"."mfa_audit" ADD COLUMN "notify_attempted_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "auth"."mfa_audit" ADD COLUMN "notified_at" timestamp with time zone;
--> statement-breakpoint
CREATE INDEX "auth_mfa_notification_pending" ON "auth"."mfa_audit" (occurred_at)
WHERE notification IS NOT NULL AND notified_at IS NULL;
--> statement-breakpoint
-- expand-only: Only new MFA setup challenges are expired and an outbox row is appended; legacy verification identifiers and passkey fields are untouched.
CREATE FUNCTION "auth"."audit_passkey_enrollment"() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE "auth"."verification" SET expires_at = to_timestamp(0) WHERE identifier IN (
        SELECT 'totp-setup:' || id FROM "auth"."session" WHERE user_id = NEW.user_id
    );
    INSERT INTO "auth"."mfa_audit" (id, user_id, action, actor, occurred_at, notification)
    VALUES ('passkey-enroll:' || NEW.id, NEW.user_id, 'mfa.passkey_enrolled',
            NEW.user_id, COALESCE(NEW.created_at, CURRENT_TIMESTAMP), 'enrolled');
    RETURN NEW;
END;
$$;
--> statement-breakpoint
-- expand-only: Only new MFA setup challenges are expired and an outbox row is appended; legacy verification identifiers and passkey fields are untouched.
CREATE TRIGGER "audit_passkey_enrollment" AFTER INSERT ON "auth"."passkey"
FOR EACH ROW EXECUTE FUNCTION "auth"."audit_passkey_enrollment"();
