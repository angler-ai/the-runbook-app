CREATE TABLE runbook.project_variables (
    org_id text NOT NULL,
    project_id text PRIMARY KEY,
    body text NOT NULL,
    revision bigint NOT NULL CHECK(revision>=0),
    FOREIGN KEY (project_id,org_id) REFERENCES runbook.project(project_id,org_id) ON DELETE CASCADE
);
CREATE TABLE runbook.directory_request (
    org_id text NOT NULL REFERENCES runbook.organization ON DELETE CASCADE,
    principal text NOT NULL,
    operation text NOT NULL,
    key text NOT NULL,
    payload text NOT NULL,
    response text NOT NULL,
    expires_at bigint NOT NULL,
    PRIMARY KEY (org_id,principal,operation,key)
);
CREATE INDEX directory_request_expiry ON runbook.directory_request(org_id,expires_at);
