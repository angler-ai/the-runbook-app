-- Archived projects (the D1 mirror of Postgres 0038_project_archived.sql).
ALTER TABLE project ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
