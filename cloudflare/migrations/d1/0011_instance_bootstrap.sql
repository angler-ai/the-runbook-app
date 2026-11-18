-- D1: 0011_instance_bootstrap.sql
-- Permanent bootstrap singleton marker.

CREATE TABLE IF NOT EXISTS instance_bootstrap (
    singleton INTEGER PRIMARY KEY NOT NULL DEFAULT 1 CHECK (singleton = 1),
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    completed_at INTEGER NOT NULL
);
