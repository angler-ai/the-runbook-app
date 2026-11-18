-- B3.1: runbook identity, its single revisioned draft, and immutable
-- versions. Publishing serializes on the runbook row lock; numbers are
-- unique per runbook and previews carry none. The version row holds the
-- semantic hash and the snapshot byte digest side by side (D21).
CREATE TABLE runbook.runbook (
    org_id text NOT NULL,
    project_id text NOT NULL,
    runbook_id text PRIMARY KEY,
    slug text NOT NULL,
    origin text NOT NULL CHECK (origin IN ('ui', 'git')),
    archived boolean NOT NULL,
    draft_source text NOT NULL,
    draft_revision bigint NOT NULL CHECK (draft_revision > 0),
    UNIQUE (org_id, project_id, slug)
);
CREATE TABLE runbook.runbook_version (
    org_id text NOT NULL,
    project_id text NOT NULL,
    runbook_id text NOT NULL REFERENCES runbook.runbook ON DELETE CASCADE,
    version_id text PRIMARY KEY,
    number integer CHECK (number > 0),
    branch text,
    commit_sha text,
    semantic_hash bytea NOT NULL CHECK (octet_length(semantic_hash) = 32),
    snapshot_digest bytea NOT NULL CHECK (octet_length(snapshot_digest) = 32),
    snapshot_size bigint NOT NULL CHECK (snapshot_size >= 0),
    source text NOT NULL,
    pins text NOT NULL,
    UNIQUE (runbook_id, number)
);
CREATE INDEX runbook_version_latest ON runbook.runbook_version (runbook_id, number DESC)
    WHERE number IS NOT NULL;
