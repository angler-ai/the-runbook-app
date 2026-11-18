-- Profile photos and project images (2026-11-17): a PNG or JPEG of at most
-- 512x512 and 1 MiB, kept in the database. A person's row may point at
-- their Google photo (source_url) instead, until they upload their own.
-- An invitation may carry the invitee's name for the account it creates.
CREATE TABLE runbook.user_picture (
    user_id text PRIMARY KEY,
    content_type text NOT NULL,
    bytes bytea NOT NULL,
    source_url text,
    updated_at bigint NOT NULL
);
CREATE TABLE runbook.project_picture (
    org_id text NOT NULL,
    project_id text NOT NULL,
    content_type text NOT NULL,
    bytes bytea NOT NULL,
    updated_at bigint NOT NULL,
    PRIMARY KEY (org_id, project_id)
);
ALTER TABLE runbook.invitation ADD COLUMN name text;
