-- Settings publishers and admissions share a transaction fence. The scheduler
-- retains read-only table grants for the settings it consumes.
CREATE FUNCTION lock_budget_settings_write() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_advisory_xact_lock(7419004);
  RETURN NULL;
END;
$$;
CREATE TRIGGER budget_policy_write AFTER INSERT OR UPDATE OR DELETE ON policy
  FOR EACH STATEMENT EXECUTE FUNCTION lock_budget_settings_write();
CREATE TRIGGER budget_group_write AFTER INSERT OR DELETE OR UPDATE OF concurrency, cost_per_day_usd, cost_per_month_usd, alert_threshold_percent, compute_rate ON groups
  FOR EACH STATEMENT EXECUTE FUNCTION lock_budget_settings_write();
CREATE TRIGGER budget_agent_write AFTER INSERT OR UPDATE OF current_version_id, status OR DELETE ON agents
  FOR EACH STATEMENT EXECUTE FUNCTION lock_budget_settings_write();
CREATE TRIGGER budget_environment_write AFTER INSERT OR UPDATE OR DELETE ON environment_versions
  FOR EACH STATEMENT EXECUTE FUNCTION lock_budget_settings_write();

ALTER TABLE budget_waiting ADD COLUMN cap numeric(14,6);
ALTER TABLE ledger_buckets ADD COLUMN closed_cap_usd numeric(14,6),
  ADD COLUMN overrun_usd numeric(14,6) NOT NULL DEFAULT 0;
ALTER TABLE ledger_reservations ADD COLUMN overrun_usd numeric(14,6) NOT NULL DEFAULT 0;
ALTER TABLE budget_event_outbox ADD COLUMN occurred_at_ms bigint NOT NULL
  DEFAULT ((extract(epoch FROM clock_timestamp()) * 1000)::bigint);
CREATE INDEX ledger_budget_waiting_identity ON budget_waiting (scope, budget_key, period, step_run_id);
CREATE INDEX ledger_usage_budget_step ON usage_events (project_id, step_run_id, created_at);
CREATE INDEX ledger_slot_budget_run ON slots (execution_id, project_id) WHERE execution_id IS NOT NULL;
