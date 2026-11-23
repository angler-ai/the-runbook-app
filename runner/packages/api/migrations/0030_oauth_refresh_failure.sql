ALTER TABLE credential_oauth ADD COLUMN refresh_failure_since timestamptz,
  ADD COLUMN refresh_failure_warned_at timestamptz;

ALTER TABLE budget_event_outbox DROP CONSTRAINT budget_event_outbox_kind_check;
ALTER TABLE budget_event_outbox ADD CONSTRAINT budget_event_outbox_kind_check
  CHECK (kind IN ('runner.budget_threshold', 'runner.probe_succeeded', 'runner.decision_cost', 'runner.task_cost',
    'credential.reconnect_required', 'credential.oauth_expiring', 'credential.oauth_refresh_failing'));
