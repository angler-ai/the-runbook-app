ALTER TABLE credentials ADD COLUMN deleted_at timestamptz;
DROP INDEX credentials_org_name;
DROP INDEX credentials_project_name;
CREATE UNIQUE INDEX credentials_org_name ON credentials (name) WHERE scope = 'org' AND deleted_at IS NULL;
CREATE UNIQUE INDEX credentials_project_name ON credentials (project_id, name) WHERE scope = 'project' AND deleted_at IS NULL;
