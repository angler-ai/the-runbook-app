-- F36 (review 2026-11-11): every foreign key gets an index leading with its
-- key, so cascades and key checks never scan (terminal commits read
-- credential_family by execution under the head lock; run deletion cascades
-- from execution_owner), and the webhook purge reads events by age.
CREATE INDEX credential_family_execution ON runbook.credential_family (execution_id);
CREATE INDEX execution_credential_execution ON runbook.execution_credential (execution_id);
CREATE INDEX start_idempotency_execution ON runbook.start_idempotency (execution_id);
CREATE INDEX start_replay_execution ON runbook.start_replay (execution_id);
CREATE INDEX work_lease_execution ON runbook.work_lease (execution_id);
CREATE INDEX admission_pool_member ON runbook.admission (org_id, project_id, key);
CREATE INDEX webhook_delivery_event
    ON runbook.webhook_delivery (org_id, project_id, event_id);
CREATE INDEX webhook_event_age ON runbook.webhook_event (org_id, project_id, created_at_ms);
CREATE INDEX org_user_user ON runbook.org_user (user_id);
CREATE INDEX project_member_org_user ON runbook.project_member (user_id, org_id);
CREATE INDEX team_member_org_user ON runbook.team_member (user_id, org_id);
CREATE INDEX verified_email_user ON runbook.verified_email (user_id);
CREATE INDEX owner_recovery_audit_user ON runbook.owner_recovery_audit (user_id);
