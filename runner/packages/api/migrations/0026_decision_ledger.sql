ALTER TABLE ledger_reservations ADD COLUMN decision_lease_id text;
CREATE INDEX ledger_reservations_decision_lease ON ledger_reservations (decision_lease_id)
  WHERE decision_lease_id IS NOT NULL;
ALTER TABLE budget_event_outbox DROP CONSTRAINT budget_event_outbox_kind_check;
ALTER TABLE budget_event_outbox ADD CONSTRAINT budget_event_outbox_kind_check
  CHECK (kind IN ('runner.budget_threshold', 'runner.probe_succeeded', 'runner.decision_cost'));
