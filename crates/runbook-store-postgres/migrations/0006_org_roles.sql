-- D29: legacy and ownerless directory fixtures remain valid; no privilege backfill.
ALTER TABLE runbook.org_user ADD COLUMN org_role text NOT NULL DEFAULT 'member'
    CHECK (org_role IN ('owner', 'admin', 'member'));
