-- D1: 0006_org_roles.sql
-- Add org_role column to org_user table.

ALTER TABLE org_user ADD COLUMN org_role TEXT NOT NULL DEFAULT 'member'
    CHECK (org_role IN ('owner', 'admin', 'member'));
