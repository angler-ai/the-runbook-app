ALTER TABLE slots
  ADD COLUMN agent_version_id text,
  ADD COLUMN claimed_at timestamptz,
  ADD COLUMN sandbox_started_at timestamptz,
  ADD COLUMN sandbox_ready_at timestamptz,
  ADD COLUMN sandbox_stopped_at timestamptz,
  ADD COLUMN outcome text CHECK (outcome IN ('succeeded', 'failed', 'blocked', 'skipped')),
  ADD COLUMN reported_at timestamptz,
  ADD COLUMN idle_stop boolean NOT NULL DEFAULT false;

CREATE INDEX slots_agent_activity ON slots(project_id, agent_id, reported_at);
CREATE INDEX slots_agent_version_runs ON slots(project_id, agent_id, agent_version_id, execution_id)
  WHERE agent_version_id IS NOT NULL;
CREATE INDEX slots_sandbox_history ON slots(sandbox_id) WHERE sandbox_id IS NOT NULL;

ALTER TABLE profiles ADD COLUMN verified_at timestamptz;
