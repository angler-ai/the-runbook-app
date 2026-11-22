CREATE TABLE "auth"."mfa_audit" (
	"id" text PRIMARY KEY NOT NULL,
	"user_id" text NOT NULL,
	"action" text NOT NULL,
	"actor" text NOT NULL,
	"occurred_at" timestamp with time zone NOT NULL
);
--> statement-breakpoint
ALTER TABLE "auth"."oauth_access_token" ADD COLUMN "mfa_verified_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "auth"."oauth_access_token" ADD COLUMN "mfa_method" text;--> statement-breakpoint
ALTER TABLE "auth"."oauth_refresh_token" ADD COLUMN "mfa_verified_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "auth"."oauth_refresh_token" ADD COLUMN "mfa_method" text;--> statement-breakpoint
CREATE INDEX "auth_mfa_audit_user_idx" ON "auth"."mfa_audit" USING btree ("user_id","occurred_at");
--> statement-breakpoint
-- expand-only: OAuth inserts keep their original fields and constraints; only the new nullable MFA snapshot columns are populated.
CREATE FUNCTION "auth"."snapshot_oauth_mfa"() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'oauth_access_token' THEN
        SELECT mfa_verified_at, mfa_method INTO NEW.mfa_verified_at, NEW.mfa_method
        FROM "auth"."oauth_refresh_token" WHERE id = NEW.refresh_id;
    ELSIF NEW.authorization_code_id IS NOT NULL THEN
        SELECT mfa_verified_at, mfa_method INTO NEW.mfa_verified_at, NEW.mfa_method
        FROM "auth"."oauth_refresh_token"
        WHERE authorization_code_id = NEW.authorization_code_id
          AND client_id = NEW.client_id AND user_id = NEW.user_id
        LIMIT 1;
    END IF;
    IF NEW.mfa_verified_at IS NULL THEN
        SELECT mfa_verified_at, mfa_method INTO NEW.mfa_verified_at, NEW.mfa_method
        FROM "auth"."session" WHERE id = NEW.session_id AND user_id = NEW.user_id;
    END IF;
    RETURN NEW;
END;
$$;
--> statement-breakpoint
-- expand-only: OAuth inserts keep their original fields and constraints; only the new nullable MFA snapshot columns are populated.
CREATE TRIGGER "snapshot_refresh_mfa" BEFORE INSERT ON "auth"."oauth_refresh_token"
FOR EACH ROW EXECUTE FUNCTION "auth"."snapshot_oauth_mfa"();
--> statement-breakpoint
-- expand-only: OAuth inserts keep their original fields and constraints; only the new nullable MFA snapshot columns are populated.
CREATE TRIGGER "snapshot_access_mfa" BEFORE INSERT ON "auth"."oauth_access_token"
FOR EACH ROW EXECUTE FUNCTION "auth"."snapshot_oauth_mfa"();
