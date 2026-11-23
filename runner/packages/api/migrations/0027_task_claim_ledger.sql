ALTER TABLE ledger_reservations ADD COLUMN slot_id text;

-- Associate charges already reserved on open slots before the upgrade.
-- Decision leases already have their own durable link.
UPDATE ledger_reservations r SET slot_id = s.id
FROM slots s
WHERE s.released_at IS NULL AND s.claim_id IS NOT NULL
  AND r.kind IN ('model', 'compute') AND r.decision_lease_id IS NULL
  AND r.step_run_id = s.step_run_id AND r.runner_id = s.runner_id
  AND r.created_at >= s.created_at;

CREATE INDEX ledger_reservations_slot ON ledger_reservations (slot_id)
  WHERE slot_id IS NOT NULL;

ALTER TABLE budget_event_outbox DROP CONSTRAINT budget_event_outbox_kind_check;
ALTER TABLE budget_event_outbox ADD CONSTRAINT budget_event_outbox_kind_check
  CHECK (kind IN ('runner.budget_threshold', 'runner.probe_succeeded', 'runner.decision_cost', 'runner.task_cost'));
