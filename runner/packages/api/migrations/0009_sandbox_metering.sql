ALTER TABLE usage_events
  ADD COLUMN vcpu_seconds numeric(18,6) NOT NULL DEFAULT 0 CHECK (vcpu_seconds >= 0),
  ADD COLUMN gib_seconds numeric(18,6) NOT NULL DEFAULT 0 CHECK (gib_seconds >= 0);

ALTER TABLE budget_event_outbox
  ADD COLUMN kind text NOT NULL DEFAULT 'runner.budget_threshold'
    CHECK (kind IN ('runner.budget_threshold', 'runner.probe_succeeded'));
