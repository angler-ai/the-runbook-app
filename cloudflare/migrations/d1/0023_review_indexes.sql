-- D1: 0023_review_indexes.sql
-- Foreign key indexing for cascade and lookup performance on D1 tables.
-- ExecutionDO foreign key indexes (credential_family, execution_credential,
-- work_lease) belong in ExecutionDO SQLite storage.

CREATE INDEX IF NOT EXISTS start_idempotency_execution ON start_idempotency (execution_id);
CREATE INDEX IF NOT EXISTS start_replay_execution ON start_replay (execution_id);
CREATE INDEX IF NOT EXISTS admission_pool_member ON admission (org_id, project_id, key);
CREATE INDEX IF NOT EXISTS webhook_delivery_event
    ON webhook_delivery (org_id, project_id, event_id);
CREATE INDEX IF NOT EXISTS webhook_event_age ON webhook_event (org_id, project_id, created_at_ms);
CREATE INDEX IF NOT EXISTS org_user_user ON org_user (user_id);
CREATE INDEX IF NOT EXISTS project_member_org_user ON project_member (user_id, org_id);
CREATE INDEX IF NOT EXISTS team_member_org_user ON team_member (user_id, org_id);
CREATE INDEX IF NOT EXISTS verified_email_user ON verified_email (user_id);
CREATE INDEX IF NOT EXISTS owner_recovery_audit_user ON owner_recovery_audit (user_id);
