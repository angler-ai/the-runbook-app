-- Profile photos, project images and invitation names (the D1 mirror of
-- Postgres 0037_pictures_invitation_name.sql).
CREATE TABLE user_picture (
    user_id TEXT PRIMARY KEY,
    content_type TEXT NOT NULL,
    bytes BLOB NOT NULL,
    source_url TEXT,
    updated_at INTEGER NOT NULL
);
CREATE TABLE project_picture (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    content_type TEXT NOT NULL,
    bytes BLOB NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (org_id, project_id)
);
ALTER TABLE invitation ADD COLUMN name TEXT;
