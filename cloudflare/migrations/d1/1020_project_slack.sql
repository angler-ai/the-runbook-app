-- SLACK1 stage 1: one incoming-webhook URL per project (the D1 mirror
-- of Postgres 0036_project_slack.sql).
CREATE TABLE project_slack (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    url TEXT NOT NULL,
    PRIMARY KEY (org_id, project_id)
);
