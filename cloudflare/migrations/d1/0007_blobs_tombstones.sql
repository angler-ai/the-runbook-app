-- D1: 0007_blobs_tombstones.sql
-- In the Cloudflare edition, blob bytes and metadata live in R2 (D56/D57).
-- Only execution tombstones are stored in D1.

CREATE TABLE IF NOT EXISTS tombstone (
    execution_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    body TEXT NOT NULL
);
