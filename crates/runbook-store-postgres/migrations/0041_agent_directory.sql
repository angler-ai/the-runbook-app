-- R0-3 (runner design §6.3, §12.3): the agent directory the runner syncs,
-- and each project's default runner agent. An agent's name is unique in
-- its project; references elsewhere use its ID.
CREATE TABLE runbook.runner_agents (
    install_id text NOT NULL REFERENCES runbook.runner_installs (id) ON DELETE CASCADE,
    agent_id text NOT NULL,
    org_id text NOT NULL,
    project_id text NOT NULL,
    group_id text NOT NULL,
    name text NOT NULL,
    status text NOT NULL CHECK (status IN ('active', 'paused', 'archived')),
    version_id text NOT NULL,
    ceiling text NOT NULL,
    updated_at_ms bigint NOT NULL,
    PRIMARY KEY (install_id, agent_id)
);

CREATE UNIQUE INDEX runner_agents_project_name ON runbook.runner_agents (project_id, name);

CREATE INDEX runner_agents_org_agent ON runbook.runner_agents (org_id, agent_id);

CREATE TABLE runbook.project_runner_settings (
    org_id text NOT NULL,
    project_id text NOT NULL,
    default_agent text,
    PRIMARY KEY (org_id, project_id)
);
