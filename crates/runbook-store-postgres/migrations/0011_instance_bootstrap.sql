-- A permanent once-only marker, independent of later directory changes.
CREATE TABLE runbook.instance_bootstrap (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    org_id text NOT NULL,
    user_id text NOT NULL,
    project_id text NOT NULL,
    completed_at timestamptz NOT NULL DEFAULT now()
);
