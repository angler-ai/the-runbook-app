-- D1: 1010_runbook_source_digests.sql (CF11)
-- Large runbook source out of D1: a draft's or version's source is stored in
-- the project's content-addressed blobs (R2, keyed by its SHA-256, beside the
-- snapshots and skill files) and the row keeps its digest and size, with the
-- text column empty. A row whose digest is NULL holds its text inline, as
-- every row written before this migration does
-- (crates/runbook-store-d1/src/sources.rs).
ALTER TABLE runbook ADD COLUMN draft_digest BLOB CHECK (draft_digest IS NULL OR length(draft_digest) = 32);
ALTER TABLE runbook ADD COLUMN draft_size INTEGER CHECK (draft_size IS NULL OR draft_size >= 0);
ALTER TABLE runbook_version ADD COLUMN source_digest BLOB CHECK (source_digest IS NULL OR length(source_digest) = 32);
ALTER TABLE runbook_version ADD COLUMN source_size INTEGER CHECK (source_size IS NULL OR source_size >= 0);
ALTER TABLE draft ADD COLUMN source_digest BLOB CHECK (source_digest IS NULL OR length(source_digest) = 32);
ALTER TABLE draft ADD COLUMN source_size INTEGER CHECK (source_size IS NULL OR source_size >= 0);
