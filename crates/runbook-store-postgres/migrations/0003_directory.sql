-- B2.2b: the directory. Identity rows are global; membership and activity are
-- per organization. Slugs are unique per organization. Every scoped table
-- carries org_id, and snapshot queries filter on it.

CREATE TABLE runbook.organization (
    org_id text PRIMARY KEY,
    title text NOT NULL
);
-- One revision row per organization. A commit locks this row, compares the
-- expected revision, applies its mutations and bumps it, in one transaction.
CREATE TABLE runbook.directory_revision (
    org_id text PRIMARY KEY REFERENCES runbook.organization ON DELETE CASCADE,
    revision bigint NOT NULL CHECK (revision >= 0)
);
CREATE TABLE runbook.user_identity (
    user_id text PRIMARY KEY,
    name text NOT NULL
);
CREATE TABLE runbook.org_user (
    org_id text NOT NULL REFERENCES runbook.organization ON DELETE CASCADE,
    user_id text NOT NULL REFERENCES runbook.user_identity,
    name text NOT NULL,
    active boolean NOT NULL,
    PRIMARY KEY (org_id, user_id)
);
CREATE TABLE runbook.project (
    project_id text PRIMARY KEY,
    org_id text NOT NULL REFERENCES runbook.organization ON DELETE CASCADE,
    slug text NOT NULL,
    title text NOT NULL,
    UNIQUE (org_id, slug)
);
CREATE TABLE runbook.team (
    team_id text PRIMARY KEY,
    org_id text NOT NULL REFERENCES runbook.organization ON DELETE CASCADE,
    slug text NOT NULL,
    title text NOT NULL,
    UNIQUE (org_id, slug)
);
CREATE TABLE runbook.project_member (
    org_id text NOT NULL,
    project_id text NOT NULL REFERENCES runbook.project ON DELETE CASCADE,
    user_id text NOT NULL,
    role text NOT NULL,
    PRIMARY KEY (project_id, user_id),
    FOREIGN KEY (org_id, user_id) REFERENCES runbook.org_user ON DELETE CASCADE
);
CREATE TABLE runbook.team_member (
    org_id text NOT NULL,
    team_id text NOT NULL REFERENCES runbook.team ON DELETE CASCADE,
    user_id text NOT NULL,
    PRIMARY KEY (team_id, user_id),
    FOREIGN KEY (org_id, user_id) REFERENCES runbook.org_user ON DELETE CASCADE
);
-- Keys are normalized by the adapter exactly as the memory directory does.
CREATE TABLE runbook.verified_email (
    email text PRIMARY KEY,
    user_id text NOT NULL REFERENCES runbook.user_identity
);
