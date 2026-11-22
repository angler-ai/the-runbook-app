CREATE TABLE `auth_mfa_audit` (
	`id` text PRIMARY KEY NOT NULL,
	`user_id` text NOT NULL,
	`action` text NOT NULL,
	`actor` text NOT NULL,
	`occurred_at` integer NOT NULL
);
--> statement-breakpoint
CREATE INDEX `auth_mfa_audit_user_idx` ON `auth_mfa_audit` (`user_id`,`occurred_at`);--> statement-breakpoint
ALTER TABLE `auth_oauth_access_token` ADD `mfa_verified_at` integer;--> statement-breakpoint
ALTER TABLE `auth_oauth_access_token` ADD `mfa_method` text;--> statement-breakpoint
ALTER TABLE `auth_oauth_refresh_token` ADD `mfa_verified_at` integer;--> statement-breakpoint
ALTER TABLE `auth_oauth_refresh_token` ADD `mfa_method` text;
--> statement-breakpoint
-- expand-only: OAuth inserts keep their original fields and constraints; only the new nullable MFA snapshot columns are populated.
CREATE TRIGGER auth_snapshot_refresh_mfa AFTER INSERT ON auth_oauth_refresh_token
BEGIN
    UPDATE auth_oauth_refresh_token
    SET (mfa_verified_at, mfa_method) = (
        SELECT mfa_verified_at, mfa_method FROM (
            SELECT mfa_verified_at, mfa_method, 0 AS priority
            FROM auth_oauth_refresh_token
            WHERE authorization_code_id = NEW.authorization_code_id
              AND client_id = NEW.client_id AND user_id = NEW.user_id AND id <> NEW.id
            UNION ALL
            SELECT mfa_verified_at, mfa_method, 1 AS priority
            FROM auth_session WHERE id = NEW.session_id AND user_id = NEW.user_id
        ) WHERE mfa_verified_at IS NOT NULL ORDER BY priority LIMIT 1
    ) WHERE id = NEW.id;
END;
--> statement-breakpoint
-- expand-only: OAuth inserts keep their original fields and constraints; only the new nullable MFA snapshot columns are populated.
CREATE TRIGGER auth_snapshot_access_mfa AFTER INSERT ON auth_oauth_access_token
BEGIN
    UPDATE auth_oauth_access_token
    SET (mfa_verified_at, mfa_method) = (
        SELECT mfa_verified_at, mfa_method FROM (
            SELECT mfa_verified_at, mfa_method, 0 AS priority
            FROM auth_oauth_refresh_token WHERE id = NEW.refresh_id
            UNION ALL
            SELECT mfa_verified_at, mfa_method, 1 AS priority
            FROM auth_session WHERE id = NEW.session_id AND user_id = NEW.user_id
        ) WHERE mfa_verified_at IS NOT NULL ORDER BY priority LIMIT 1
    ) WHERE id = NEW.id;
END;
