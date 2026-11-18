-- D1: 0004_directory_member_org.sql
-- Pin membership rows to the project or team's organization in the schema.

CREATE UNIQUE INDEX IF NOT EXISTS project_id_org_unique ON project (project_id, org_id);
CREATE UNIQUE INDEX IF NOT EXISTS team_id_org_unique ON team (team_id, org_id);

CREATE TABLE IF NOT EXISTS project_member_new (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL,
    PRIMARY KEY (project_id, user_id),
    FOREIGN KEY (project_id, org_id) REFERENCES project (project_id, org_id) ON DELETE CASCADE,
    FOREIGN KEY (org_id, user_id) REFERENCES org_user (org_id, user_id) ON DELETE CASCADE
);
INSERT INTO project_member_new SELECT org_id, project_id, user_id, role FROM project_member;
DROP TABLE project_member;
ALTER TABLE project_member_new RENAME TO project_member;

CREATE TABLE IF NOT EXISTS team_member_new (
    org_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    PRIMARY KEY (team_id, user_id),
    FOREIGN KEY (team_id, org_id) REFERENCES team (team_id, org_id) ON DELETE CASCADE,
    FOREIGN KEY (org_id, user_id) REFERENCES org_user (org_id, user_id) ON DELETE CASCADE
);
INSERT INTO team_member_new SELECT org_id, team_id, user_id FROM team_member;
DROP TABLE team_member;
ALTER TABLE team_member_new RENAME TO team_member;
