CREATE TABLE `auth_passkey` (
	`id` text PRIMARY KEY NOT NULL,
	`name` text,
	`public_key` text NOT NULL,
	`user_id` text NOT NULL,
	`credential_id` text NOT NULL,
	`counter` integer NOT NULL,
	`device_type` text NOT NULL,
	`backed_up` integer NOT NULL,
	`transports` text,
	`created_at` integer,
	`last_used_at` integer,
	`aaguid` text,
	FOREIGN KEY (`user_id`) REFERENCES `auth_user`(`id`) ON UPDATE no action ON DELETE cascade
);
--> statement-breakpoint
CREATE UNIQUE INDEX `auth_passkey_credential_id_unique` ON `auth_passkey` (`credential_id`);--> statement-breakpoint
CREATE INDEX `auth_passkey_user_id_idx` ON `auth_passkey` (`user_id`);--> statement-breakpoint
CREATE TABLE `auth_two_factor` (
	`id` text PRIMARY KEY NOT NULL,
	`secret` text NOT NULL,
	`backup_codes` text NOT NULL,
	`user_id` text NOT NULL,
	`verified` integer DEFAULT true,
	`failed_verification_count` integer DEFAULT 0,
	`locked_until` integer,
	FOREIGN KEY (`user_id`) REFERENCES `auth_user`(`id`) ON UPDATE no action ON DELETE cascade
);
--> statement-breakpoint
CREATE UNIQUE INDEX `auth_two_factor_user_id_unique` ON `auth_two_factor` (`user_id`);--> statement-breakpoint
ALTER TABLE `auth_session` ADD `mfa_verified_at` integer;--> statement-breakpoint
ALTER TABLE `auth_session` ADD `mfa_method` text;--> statement-breakpoint
ALTER TABLE `auth_user` ADD `two_factor_enabled` integer DEFAULT false NOT NULL;