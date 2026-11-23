-- R0-2 (runner design §6.4, §12): the runner registry. One install
-- (control plane) per organization, its groups and its runners. Every
-- runner secret is stored as its SHA-256 only.
CREATE TABLE runbook.runner_bootstrap_tokens (
    token_hash bytea PRIMARY KEY,
    org_id text NOT NULL,
    url text NOT NULL,
    created_at_ms bigint NOT NULL,
    expires_at_ms bigint NOT NULL,
    used_at_ms bigint
);

CREATE INDEX runner_bootstrap_tokens_org ON runbook.runner_bootstrap_tokens (org_id);

CREATE TABLE runbook.runner_installs (
    id text PRIMARY KEY,
    org_id text NOT NULL,
    url text NOT NULL,
    version text NOT NULL,
    contract_version integer NOT NULL,
    directory_token_hash bytea NOT NULL UNIQUE,
    release_token_hash bytea NOT NULL UNIQUE,
    join_token_hash bytea NOT NULL UNIQUE,
    created_at_ms bigint NOT NULL,
    last_heartbeat_at_ms bigint
);

CREATE UNIQUE INDEX runner_installs_one_per_org ON runbook.runner_installs (org_id);

CREATE TABLE runbook.runner_groups (
    id text PRIMARY KEY,
    install_id text NOT NULL REFERENCES runbook.runner_installs (id) ON DELETE CASCADE,
    org_id text NOT NULL,
    name text NOT NULL
);

CREATE INDEX runner_groups_install ON runbook.runner_groups (install_id);

CREATE TABLE runbook.runner_group_projects (
    group_id text NOT NULL REFERENCES runbook.runner_groups (id) ON DELETE CASCADE,
    project_id text NOT NULL,
    PRIMARY KEY (group_id, project_id)
);

CREATE TABLE runbook.runner_runners (
    id text PRIMARY KEY,
    install_id text NOT NULL REFERENCES runbook.runner_installs (id) ON DELETE CASCADE,
    group_id text NOT NULL REFERENCES runbook.runner_groups (id) ON DELETE CASCADE,
    org_id text NOT NULL,
    name text NOT NULL,
    version text NOT NULL,
    claim_token_hash bytea NOT NULL UNIQUE,
    status text NOT NULL DEFAULT 'joined',
    running integer NOT NULL DEFAULT 0,
    capacity integer NOT NULL DEFAULT 0,
    created_at_ms bigint NOT NULL,
    last_heartbeat_at_ms bigint,
    UNIQUE (group_id, name)
);

CREATE INDEX runner_runners_install ON runbook.runner_runners (install_id);
