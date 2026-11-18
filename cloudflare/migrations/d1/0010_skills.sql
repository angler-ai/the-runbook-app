-- D1: 0010_skills.sql
-- Project skill versions. File bytes live in R2; metadata and identity live in D1.

CREATE TABLE IF NOT EXISTS skill_version (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    hash BLOB NOT NULL CHECK (length(hash) = 32),
    description TEXT NOT NULL,
    manifest TEXT NOT NULL,
    provenance TEXT NOT NULL,
    seq INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (org_id, project_id, name, hash)
);
CREATE INDEX IF NOT EXISTS skill_version_latest
    ON skill_version (org_id, project_id, name, seq DESC);
CREATE INDEX IF NOT EXISTS skill_version_hash ON skill_version (org_id, project_id, hash);
