-- B3.2b: immutable project skill versions. File bytes live in blob chunks
-- under per-file digests; this row records identity, manifest and
-- provenance. seq orders publishes so latest-per-name is well defined.
CREATE TABLE runbook.skill_version (
    org_id text NOT NULL,
    project_id text NOT NULL,
    name text NOT NULL,
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    description text NOT NULL,
    manifest text NOT NULL,
    provenance text NOT NULL,
    seq bigint GENERATED ALWAYS AS IDENTITY,
    PRIMARY KEY (org_id, project_id, name, hash)
);
CREATE INDEX skill_version_latest
    ON runbook.skill_version (org_id, project_id, name, seq DESC);
CREATE INDEX skill_version_hash ON runbook.skill_version (org_id, project_id, hash);
