-- R0-4 (runner design §8.3): the runner's ready listing. One row per
-- ready step aimed at a runner agent, written in the execution commit
-- from the run's projection; the runner sweeps it by group.
CREATE TABLE runner_ready_steps (
    step_run_id TEXT NOT NULL PRIMARY KEY,
    execution_id TEXT NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    ready_at_ms INTEGER NOT NULL,
    -- The step's max_cost_usd in whole micro-dollars.
    max_cost_micro_usd INTEGER
);

CREATE INDEX runner_ready_steps_execution ON runner_ready_steps (execution_id);

CREATE INDEX runner_ready_steps_agent ON runner_ready_steps (org_id, agent_id, ready_at_ms);
