-- R0-3 (runner design §6.3, §12.3): the agent directory the runner syncs,
-- and each project's default runner agent. An agent's name is unique in
-- its project; references elsewhere use its ID.
CREATE TABLE runner_agents (
    install_id TEXT NOT NULL REFERENCES runner_installs (id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    name TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active', 'paused', 'archived')),
    version_id TEXT NOT NULL,
    ceiling TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (install_id, agent_id)
);

CREATE UNIQUE INDEX runner_agents_project_name ON runner_agents (project_id, name);

CREATE INDEX runner_agents_org_agent ON runner_agents (org_id, agent_id);

CREATE TABLE project_runner_settings (
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    default_agent TEXT,
    PRIMARY KEY (org_id, project_id)
);
