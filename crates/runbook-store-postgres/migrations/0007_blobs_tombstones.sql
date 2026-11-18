-- B2.4: chunked blobs and execution tombstones. A blob key is the exact
-- JSON of its typed BlobKey; shared keys address snapshots by the SHA-256
-- of their bytes (D21), execution keys carry the run and a name.
CREATE TABLE runbook.blob (
    org_id text NOT NULL,
    project_id text NOT NULL,
    key text NOT NULL,
    execution_id text REFERENCES runbook.execution_owner ON DELETE CASCADE,
    name text,
    hash bytea,
    CHECK ((execution_id IS NULL) = (hash IS NOT NULL)),
    CHECK (hash IS NULL OR octet_length(hash) = 32),
    PRIMARY KEY (org_id, project_id, key)
);
CREATE INDEX blob_execution ON runbook.blob (execution_id)
    WHERE execution_id IS NOT NULL;
CREATE TABLE runbook.blob_chunk (
    org_id text NOT NULL,
    project_id text NOT NULL,
    key text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    bytes bytea NOT NULL,
    PRIMARY KEY (org_id, project_id, key, ordinal),
    FOREIGN KEY (org_id, project_id, key)
        REFERENCES runbook.blob (org_id, project_id, key) ON DELETE CASCADE
);
-- One tombstone per deleted execution; commits and admissions for its ID
-- are rejected from then on.
CREATE TABLE runbook.tombstone (
    execution_id text PRIMARY KEY,
    org_id text NOT NULL,
    project_id text NOT NULL,
    body text NOT NULL
);
