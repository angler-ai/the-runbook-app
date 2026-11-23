ALTER TABLE "auth"."passkey" ADD COLUMN "mfa_generation" integer DEFAULT -1 NOT NULL;--> statement-breakpoint
ALTER TABLE "auth"."session" ADD COLUMN "mfa_generation" integer DEFAULT -1 NOT NULL;--> statement-breakpoint
ALTER TABLE "auth"."user" ADD COLUMN "mfa_generation" integer DEFAULT 0 NOT NULL;
--> statement-breakpoint
-- expand-only: Omitted generations use the reserved default and are stamped under the account lock; only explicit stale generations are rejected.
CREATE FUNCTION "auth"."guard_mfa_generation"() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE current_generation integer;
BEGIN
    -- Use the same account lock as a reset. If this write wins, reset sees and
    -- deletes it. If reset wins, the verified ceremony carries an old generation.
    SELECT mfa_generation INTO current_generation FROM "auth"."user"
    WHERE id = NEW.user_id FOR NO KEY UPDATE;
    IF NEW.mfa_generation = -1 THEN
        NEW.mfa_generation := current_generation;
    ELSIF current_generation IS DISTINCT FROM NEW.mfa_generation THEN
        RAISE EXCEPTION 'MFA generation changed';
    END IF;
    RETURN NEW;
END;
$$;
--> statement-breakpoint
-- expand-only: Legacy inserts omit the new generation column and retain a valid write after an account reset.
CREATE TRIGGER "guard_passkey_generation" BEFORE INSERT ON "auth"."passkey"
FOR EACH ROW EXECUTE FUNCTION "auth"."guard_mfa_generation"();
--> statement-breakpoint
-- expand-only: Legacy inserts omit the new generation column and retain a valid write after an account reset.
CREATE TRIGGER "guard_session_generation" BEFORE INSERT ON "auth"."session"
FOR EACH ROW EXECUTE FUNCTION "auth"."guard_mfa_generation"();
--> statement-breakpoint
-- expand-only: Initialize the new generation column on preexisting passkey rows; all preexisting accounts start at generation zero.
UPDATE "auth"."passkey" SET mfa_generation = 0 WHERE mfa_generation = -1;
--> statement-breakpoint
-- expand-only: Initialize the new generation column on preexisting session rows; all preexisting accounts start at generation zero.
UPDATE "auth"."session" SET mfa_generation = 0 WHERE mfa_generation = -1;
