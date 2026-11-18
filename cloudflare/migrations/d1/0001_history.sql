-- D1: 0001_history.sql
-- In D1, no schemas; timestamptz becomes INTEGER (epoch ms).
CREATE TABLE IF NOT EXISTS schema_migration (
    version INTEGER PRIMARY KEY NOT NULL CHECK (version > 0),
    checksum TEXT NOT NULL,
    applied_at INTEGER NOT NULL
);
