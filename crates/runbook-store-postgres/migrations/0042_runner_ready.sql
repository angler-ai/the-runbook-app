-- R0-4 (runner design §8.3): the runner's ready listing. One row per
-- ready step aimed at a runner agent, written in the execution commit
-- from the run's projection; the runner sweeps it by group.
CREATE TABLE runbook.runner_ready_steps (
    step_run_id text PRIMARY KEY,
    execution_id text NOT NULL,
    org_id text NOT NULL,
    project_id text NOT NULL,
    agent_id text NOT NULL,
    ready_at_ms bigint NOT NULL,
    -- The step's max_cost_usd in whole micro-dollars.
    max_cost_micro_usd bigint
);

CREATE INDEX runner_ready_steps_execution ON runbook.runner_ready_steps (execution_id);

CREATE INDEX runner_ready_steps_agent ON runbook.runner_ready_steps (org_id, agent_id, ready_at_ms);
