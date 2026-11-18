-- D1: 0013_directory_api.sql
-- Project variables and directory mutation idempotency requests.

CREATE TABLE IF NOT EXISTS project_variables (
    org_id TEXT NOT NULL,
    project_id TEXT PRIMARY KEY NOT NULL,
    body TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    FOREIGN KEY (project_id, org_id) REFERENCES project (project_id, org_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS directory_request (
    org_id TEXT NOT NULL REFERENCES organization (org_id) ON DELETE CASCADE,
    principal TEXT NOT NULL,
    operation TEXT NOT NULL,
    key TEXT NOT NULL,
    payload TEXT NOT NULL,
    response TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (org_id, principal, operation, key)
);
CREATE INDEX IF NOT EXISTS directory_request_expiry ON directory_request (org_id, expires_at);
