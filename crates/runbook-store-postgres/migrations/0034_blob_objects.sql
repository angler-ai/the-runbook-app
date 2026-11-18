-- STO1: optional GCS or S3 storage for blobs.
-- Postgres remains the authority for ownership, tenancy and immutability.
-- Existing rows stay 'postgres' and keep their chunks; new rows with 'object'
-- store bytes in the bucket.
CREATE DOMAIN runbook.blob_location AS text
    CHECK (VALUE IN ('postgres', 'object'));

ALTER TABLE runbook.blob ADD COLUMN size bigint;
ALTER TABLE runbook.blob ADD COLUMN sha256 bytea;
ALTER TABLE runbook.blob ADD COLUMN location runbook.blob_location NOT NULL DEFAULT 'postgres';
