ALTER TABLE `auth_passkey` ADD `mfa_generation` integer DEFAULT -1 NOT NULL;--> statement-breakpoint
ALTER TABLE `auth_session` ADD `mfa_generation` integer DEFAULT -1 NOT NULL;--> statement-breakpoint
ALTER TABLE `auth_user` ADD `mfa_generation` integer DEFAULT 0 NOT NULL;
--> statement-breakpoint
-- expand-only: Legacy inserts omit the new generation column and retain a valid write after an account reset.
CREATE TRIGGER auth_guard_passkey_generation BEFORE INSERT ON auth_passkey
WHEN NEW.mfa_generation <> -1 AND NEW.mfa_generation IS NOT (SELECT mfa_generation FROM auth_user WHERE id = NEW.user_id)
BEGIN
    SELECT RAISE(ABORT, 'MFA generation changed');
END;
--> statement-breakpoint
-- expand-only: Legacy inserts omit the new generation column and retain a valid write after an account reset.
CREATE TRIGGER auth_guard_session_generation BEFORE INSERT ON auth_session
WHEN NEW.mfa_generation <> -1 AND NEW.mfa_generation IS NOT (SELECT mfa_generation FROM auth_user WHERE id = NEW.user_id)
BEGIN
    SELECT RAISE(ABORT, 'MFA generation changed');
END;
--> statement-breakpoint
-- expand-only: Stamp the new generation column on legacy passkey inserts, which omit it and receive the reserved default.
CREATE TRIGGER auth_stamp_passkey_generation AFTER INSERT ON auth_passkey
WHEN NEW.mfa_generation = -1
BEGIN
    UPDATE auth_passkey
    SET mfa_generation = (SELECT mfa_generation FROM auth_user WHERE id = NEW.user_id)
    WHERE id = NEW.id;
END;
--> statement-breakpoint
-- expand-only: Stamp the new generation column on legacy session inserts, which omit it and receive the reserved default.
CREATE TRIGGER auth_stamp_session_generation AFTER INSERT ON auth_session
WHEN NEW.mfa_generation = -1
BEGIN
    UPDATE auth_session
    SET mfa_generation = (SELECT mfa_generation FROM auth_user WHERE id = NEW.user_id)
    WHERE id = NEW.id;
END;
--> statement-breakpoint
-- expand-only: Initialize the new generation column on preexisting passkey rows; all preexisting accounts start at generation zero.
UPDATE auth_passkey SET mfa_generation = 0 WHERE mfa_generation = -1;
--> statement-breakpoint
-- expand-only: Initialize the new generation column on preexisting session rows; all preexisting accounts start at generation zero.
UPDATE auth_session SET mfa_generation = 0 WHERE mfa_generation = -1;
