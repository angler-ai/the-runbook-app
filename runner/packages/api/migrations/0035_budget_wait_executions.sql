-- Waits can be recorded before a step has a slot, so retain the execution
-- from the ready queue. Existing waits are refreshed on the next poll.
ALTER TABLE budget_waiting ADD COLUMN execution_id text;
