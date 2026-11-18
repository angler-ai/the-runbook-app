CREATE SCHEMA IF NOT EXISTS runbook;
CREATE TABLE IF NOT EXISTS runbook.schema_migration (
    version integer PRIMARY KEY CHECK (version > 0),
    checksum text NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now()
);
