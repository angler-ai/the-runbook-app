-- R0-2 (runner design §6.4, §12): the runner registry. One install
-- (control plane) per organization, its groups and its runners. Every
-- runner secret is stored as its SHA-256 only.
CREATE TABLE runner_bootstrap_tokens (
    token_hash BLOB NOT NULL PRIMARY KEY,
    org_id TEXT NOT NULL,
    url TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    used_at_ms INTEGER
);

CREATE INDEX runner_bootstrap_tokens_org ON runner_bootstrap_tokens (org_id);

CREATE TABLE runner_installs (
    id TEXT NOT NULL PRIMARY KEY,
    org_id TEXT NOT NULL,
    url TEXT NOT NULL,
    version TEXT NOT NULL,
    contract_version INTEGER NOT NULL,
    directory_token_hash BLOB NOT NULL UNIQUE,
    release_token_hash BLOB NOT NULL UNIQUE,
    join_token_hash BLOB NOT NULL UNIQUE,
    created_at_ms INTEGER NOT NULL,
    last_heartbeat_at_ms INTEGER
);

CREATE UNIQUE INDEX runner_installs_one_per_org ON runner_installs (org_id);

CREATE TABLE runner_groups (
    id TEXT NOT NULL PRIMARY KEY,
    install_id TEXT NOT NULL REFERENCES runner_installs (id) ON DELETE CASCADE,
    org_id TEXT NOT NULL,
    name TEXT NOT NULL
);

CREATE INDEX runner_groups_install ON runner_groups (install_id);

CREATE TABLE runner_group_projects (
    group_id TEXT NOT NULL REFERENCES runner_groups (id) ON DELETE CASCADE,
    project_id TEXT NOT NULL,
    PRIMARY KEY (group_id, project_id)
);

CREATE TABLE runner_runners (
    id TEXT NOT NULL PRIMARY KEY,
    install_id TEXT NOT NULL REFERENCES runner_installs (id) ON DELETE CASCADE,
    group_id TEXT NOT NULL REFERENCES runner_groups (id) ON DELETE CASCADE,
    org_id TEXT NOT NULL,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    claim_token_hash BLOB NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'joined',
    running INTEGER NOT NULL DEFAULT 0,
    capacity INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    last_heartbeat_at_ms INTEGER,
    UNIQUE (group_id, name)
);

CREATE INDEX runner_runners_install ON runner_runners (install_id);
