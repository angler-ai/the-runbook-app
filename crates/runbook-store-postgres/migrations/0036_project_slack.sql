-- SLACK1 stage 1: one incoming-webhook URL per project. The URL is a
-- credential; it is write-only through the API and never logged.
CREATE TABLE runbook.project_slack (
    org_id text NOT NULL,
    project_id text NOT NULL,
    url text NOT NULL,
    PRIMARY KEY (org_id, project_id)
);
