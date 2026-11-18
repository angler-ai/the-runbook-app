-- B2.2b review R8: pin membership rows to the project or team's
-- organization in the schema, not only in adapter code.

ALTER TABLE runbook.project
    ADD CONSTRAINT project_id_org_unique UNIQUE (project_id, org_id);
ALTER TABLE runbook.team
    ADD CONSTRAINT team_id_org_unique UNIQUE (team_id, org_id);

ALTER TABLE runbook.project_member DROP CONSTRAINT project_member_project_id_fkey;
ALTER TABLE runbook.project_member
    ADD CONSTRAINT project_member_project_org_fkey
    FOREIGN KEY (project_id, org_id)
    REFERENCES runbook.project (project_id, org_id) ON DELETE CASCADE;

ALTER TABLE runbook.team_member DROP CONSTRAINT team_member_team_id_fkey;
ALTER TABLE runbook.team_member
    ADD CONSTRAINT team_member_team_org_fkey
    FOREIGN KEY (team_id, org_id)
    REFERENCES runbook.team (team_id, org_id) ON DELETE CASCADE;
