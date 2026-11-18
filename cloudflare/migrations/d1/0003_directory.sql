-- D1: 0003_directory.sql
-- The directory: organizations, users, projects, teams, memberships and emails.

CREATE TABLE IF NOT EXISTS organization (
    org_id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS directory_revision (
    org_id TEXT PRIMARY KEY NOT NULL REFERENCES organization (org_id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision >= 0)
);

CREATE TABLE IF NOT EXISTS user_identity (
    user_id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS org_user (
    org_id TEXT NOT NULL REFERENCES organization (org_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES user_identity (user_id),
    name TEXT NOT NULL,
    active INTEGER NOT NULL,
    PRIMARY KEY (org_id, user_id)
);

CREATE TABLE IF NOT EXISTS project (
    project_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organization (org_id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    title TEXT NOT NULL,
    UNIQUE (org_id, slug)
);

CREATE TABLE IF NOT EXISTS team (
    team_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organization (org_id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    title TEXT NOT NULL,
    UNIQUE (org_id, slug)
);

CREATE TABLE IF NOT EXISTS project_member (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES project (project_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL,
    PRIMARY KEY (project_id, user_id),
    FOREIGN KEY (org_id, user_id) REFERENCES org_user (org_id, user_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS team_member (
    org_id TEXT NOT NULL,
    team_id TEXT NOT NULL REFERENCES team (team_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    PRIMARY KEY (team_id, user_id),
    FOREIGN KEY (org_id, user_id) REFERENCES org_user (org_id, user_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS verified_email (
    email TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES user_identity (user_id)
);
