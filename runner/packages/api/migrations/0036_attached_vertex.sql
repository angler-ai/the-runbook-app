-- A grant ID identifies a single confirmed destination revision. It has no
-- credential or secret-version row and rotates every time the binding changes.
ALTER TABLE providers ADD COLUMN project_id text, ADD COLUMN attached_binding_id text UNIQUE;
GRANT SELECT (type, auth, project_id, region, endpoint, attached_binding_id) ON providers TO rb_vault;
GRANT SELECT (model) ON profiles TO rb_vault;
