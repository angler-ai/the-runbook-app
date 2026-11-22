-- The runner database (plan "Runner database", design §6, §7.3, §7.9, §8).
-- Applied by the owner (`runner migrate`); each process connects with its
-- own login, a member of one group role below, with only the grants listed
-- at the end. Every execution table carries group_id and runner_id (§6.4).

DO $$
BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'rb_api') THEN CREATE ROLE rb_api NOLOGIN; END IF;
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'rb_scheduler') THEN CREATE ROLE rb_scheduler NOLOGIN; END IF;
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'rb_worker') THEN CREATE ROLE rb_worker NOLOGIN; END IF;
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'rb_vault') THEN CREATE ROLE rb_vault NOLOGIN; END IF;
END $$;

-- Control plane, runner groups and runners (§6.4).

CREATE TABLE groups (
  group_id text PRIMARY KEY,
  name text NOT NULL UNIQUE,
  -- Projects allowed on the group: mirrors the core registry (set by org admins there).
  projects jsonb NOT NULL DEFAULT '[]',
  concurrency integer,
  cost_per_day_usd numeric(14, 6),
  cost_per_month_usd numeric(14, 6),
  sandbox jsonb NOT NULL DEFAULT '{}',
  compute_rate jsonb NOT NULL DEFAULT '{}',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE runners (
  runner_id text PRIMARY KEY,
  group_id text NOT NULL REFERENCES groups (group_id),
  name text NOT NULL,
  status text NOT NULL DEFAULT 'offline',
  capacity integer NOT NULL DEFAULT 0,
  version text,
  last_heartbeat_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);

-- Organization-level configuration (§6.1). Sharing defaults to none.

CREATE TABLE providers (
  id text PRIMARY KEY,
  name text NOT NULL UNIQUE,
  type text NOT NULL CHECK (type IN ('anthropic', 'openai', 'google', 'vertex', 'bedrock', 'azure-openai', 'openai-compatible')),
  auth text NOT NULL CHECK (auth IN ('stored', 'attached')),
  credential_id text,
  endpoint text,
  region text,
  models jsonb NOT NULL DEFAULT '[]',
  sharing jsonb NOT NULL DEFAULT '{"projects": "none"}',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE profiles (
  id text PRIMARY KEY,
  name text NOT NULL UNIQUE,
  provider_id text NOT NULL REFERENCES providers (id),
  model text NOT NULL,
  options jsonb NOT NULL DEFAULT '{}',
  sharing jsonb NOT NULL DEFAULT '{"projects": "none"}',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE mcp_servers (
  id text PRIMARY KEY,
  name text NOT NULL UNIQUE,
  url text NOT NULL,
  transport text NOT NULL CHECK (transport IN ('streamable-http', 'sse')),
  credential_id text,
  -- Per-tool labels: read, write or destructive; unlabeled tools are destructive.
  tool_labels jsonb NOT NULL DEFAULT '{}',
  never_allowed jsonb NOT NULL DEFAULT '[]',
  pinned_snapshot_id text,
  sharing jsonb NOT NULL DEFAULT '{"projects": "none"}',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

-- Pinned tool snapshots are immutable: re-pinning adds a row.
CREATE TABLE mcp_tool_snapshots (
  id text PRIMARY KEY,
  server_id text NOT NULL REFERENCES mcp_servers (id),
  hash bytea NOT NULL,
  tools jsonb NOT NULL,
  pinned_at timestamptz NOT NULL DEFAULT now(),
  pinned_by text NOT NULL
);

-- Credentials (§7.2): configuration and the destination binding, written by
-- runner-api. The vault never trusts binding_hash here: it recomputes the
-- binding from config at every unwrap.
CREATE TABLE credentials (
  id text PRIMARY KEY,
  org_id text NOT NULL,
  scope text NOT NULL DEFAULT 'org' CHECK (scope IN ('org', 'project')),
  project_id text,
  name text NOT NULL,
  type text NOT NULL,
  config jsonb NOT NULL DEFAULT '{}',
  binding jsonb NOT NULL DEFAULT '{}',
  binding_hash bytea,
  label text NOT NULL DEFAULT 'destructive' CHECK (label IN ('read', 'write', 'destructive')),
  sharing jsonb NOT NULL DEFAULT '{"projects": "none"}',
  -- Runner groups it may be released to: "all" or a list of group IDs.
  groups jsonb NOT NULL DEFAULT '"all"',
  delivery jsonb NOT NULL DEFAULT '[]',
  -- Per secret field: {version, fingerprint, created_at, created_by}, from the vault's seal response.
  secret_meta jsonb NOT NULL DEFAULT '{}',
  status text NOT NULL DEFAULT 'needs_secret' CHECK (status IN ('needs_secret', 'active', 'revoked')),
  created_at timestamptz NOT NULL DEFAULT now(),
  created_by text,
  updated_at timestamptz NOT NULL DEFAULT now(),
  revoked_at timestamptz,
  CHECK ((scope = 'org') = (project_id IS NULL))
);
CREATE UNIQUE INDEX credentials_org_name ON credentials (name) WHERE scope = 'org';
CREATE UNIQUE INDEX credentials_project_name ON credentials (project_id, name) WHERE scope = 'project';

-- §7.3. Written and read only by the vault. No cascade from credentials: a
-- cascade runs as the owner and would let runner-api shred secrets.
CREATE TABLE credential_secret_versions (
  credential_id text NOT NULL REFERENCES credentials (id),
  field text NOT NULL,
  version integer NOT NULL CHECK (version > 0),
  ciphertext bytea NOT NULL,
  nonce bytea NOT NULL,
  wrapped_dek bytea,
  kek_ref text NOT NULL,
  binding_hash bytea NOT NULL,
  fingerprint text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  created_by text NOT NULL,
  -- Set when a newer version is written; the old one stays usable for live
  -- grants for 24 hours, then its wrapped data key is deleted.
  superseded_at timestamptz,
  destroyed_at timestamptz,
  PRIMARY KEY (credential_id, field, version)
);

-- HPKE ingest keys (§7.4): the private key is wrapped by the KEK.
CREATE TABLE ingest_keys (
  id text PRIMARY KEY,
  public_key bytea NOT NULL,
  wrapped_private_key bytea NOT NULL,
  kek_ref text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  rotate_after timestamptz NOT NULL,
  expires_at timestamptz NOT NULL
);

-- Vault-held keys other than ingest keys (the fingerprint HMAC key), KEK-wrapped.
CREATE TABLE vault_keys (
  purpose text PRIMARY KEY,
  wrapped bytea NOT NULL,
  kek_ref text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

-- Sealed blobs already accepted, kept 24 hours so a blob is accepted once.
CREATE TABLE seal_replay (
  digest bytea PRIMARY KEY,
  seen_at timestamptz NOT NULL DEFAULT now()
);

-- Project agents (§6.2, §10).

CREATE TABLE agents (
  id text PRIMARY KEY,
  project_id text NOT NULL,
  group_id text NOT NULL,
  name text NOT NULL,
  description text NOT NULL DEFAULT '',
  status text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'paused', 'archived')),
  current_version_id text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (project_id, name)
);

-- Every saved change is an immutable version (no UPDATE or DELETE grants).
CREATE TABLE agent_versions (
  id text PRIMARY KEY,
  agent_id text NOT NULL REFERENCES agents (id),
  version integer NOT NULL,
  -- {model, instructions, skills, mcp, credentials: [{credential_id, delivery}], egress, limits, sandbox}
  config jsonb NOT NULL,
  ceiling jsonb NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  created_by text NOT NULL,
  UNIQUE (agent_id, version)
);

CREATE TABLE proposals (
  id text PRIMARY KEY,
  kind text NOT NULL,
  target_type text NOT NULL,
  target_id text,
  diff jsonb NOT NULL,
  reason text NOT NULL DEFAULT '',
  proposed_by text NOT NULL,
  sponsor_id text,
  status text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'approved', 'rejected', 'expired')),
  created_at timestamptz NOT NULL DEFAULT now(),
  expires_at timestamptz NOT NULL,
  decided_at timestamptz,
  decided_by text
);

CREATE TABLE policy (
  id integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
  org_concurrency integer NOT NULL DEFAULT 20,
  org_cost_per_day_usd numeric(14, 6) NOT NULL DEFAULT 50,
  org_cost_per_month_usd numeric(14, 6),
  agent_concurrency integer NOT NULL DEFAULT 5,
  agent_max_cost_usd numeric(14, 6) NOT NULL DEFAULT 1,
  default_egress jsonb NOT NULL DEFAULT '[]',
  credential_max_age_days integer,
  time_zone text NOT NULL DEFAULT 'UTC',
  updated_at timestamptz NOT NULL DEFAULT now()
);

-- Cost and concurrency (§8.2–8.4).

CREATE TABLE ledger_buckets (
  id text PRIMARY KEY,
  level text NOT NULL CHECK (level IN ('step', 'run', 'agent', 'project', 'group', 'org')),
  key text NOT NULL,
  period text NOT NULL CHECK (period IN ('step', 'run', 'day', 'month')),
  period_start date,
  group_id text,
  runner_id text,
  cap_usd numeric(14, 6),
  reserved_usd numeric(14, 6) NOT NULL DEFAULT 0,
  spent_usd numeric(14, 6) NOT NULL DEFAULT 0,
  alerts_sent jsonb NOT NULL DEFAULT '[]',
  UNIQUE (level, key, period, period_start)
);

CREATE TABLE ledger_reservations (
  id text PRIMARY KEY,
  step_run_id text NOT NULL,
  group_id text NOT NULL,
  runner_id text NOT NULL,
  bucket_ids jsonb NOT NULL,
  kind text NOT NULL CHECK (kind IN ('model', 'compute', 'admission')),
  amount_usd numeric(14, 6) NOT NULL,
  actual_usd numeric(14, 6),
  status text NOT NULL DEFAULT 'reserved' CHECK (status IN ('reserved', 'settled', 'released')),
  created_at timestamptz NOT NULL DEFAULT now(),
  settled_at timestamptz
);

CREATE TABLE usage_events (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  step_run_id text NOT NULL,
  group_id text NOT NULL,
  runner_id text NOT NULL,
  project_id text NOT NULL,
  agent_id text NOT NULL,
  kind text NOT NULL CHECK (kind IN ('model', 'compute')),
  provider text,
  model text,
  input_tokens bigint NOT NULL DEFAULT 0,
  output_tokens bigint NOT NULL DEFAULT 0,
  cached_tokens bigint NOT NULL DEFAULT 0,
  cost_usd numeric(14, 6) NOT NULL DEFAULT 0,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE slots (
  id text PRIMARY KEY,
  step_run_id text NOT NULL,
  group_id text NOT NULL,
  runner_id text NOT NULL,
  project_id text NOT NULL,
  agent_id text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  released_at timestamptz,
  release_reason text,
  execution_id text,
  expires_at timestamptz,
  admission_reservation_id text REFERENCES ledger_reservations(id),
  max_cost_usd numeric(14, 6)
);

-- Observability (§7.10, §9.2).

CREATE TABLE transcripts (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  step_run_id text NOT NULL,
  group_id text NOT NULL,
  runner_id text NOT NULL,
  project_id text NOT NULL,
  agent_id text NOT NULL,
  seq integer NOT NULL,
  -- Redacted before it is written (the worker runs the core's redactor).
  entry jsonb NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tool_calls (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  step_run_id text NOT NULL,
  group_id text NOT NULL,
  runner_id text NOT NULL,
  project_id text NOT NULL,
  agent_id text NOT NULL,
  server_id text,
  tool text NOT NULL,
  args_redacted jsonb,
  duration_ms integer,
  result_bytes integer,
  error text,
  created_at timestamptz NOT NULL DEFAULT now()
);

-- One row per release (§7.5), written by the vault.
CREATE TABLE releases (
  id text PRIMARY KEY,
  step_run_id text NOT NULL,
  claim_id text,
  generation integer,
  group_id text,
  runner_id text,
  project_id text,
  agent_id text,
  agent_version text,
  credential_id text NOT NULL,
  field text NOT NULL,
  credential_version integer NOT NULL,
  delivery text NOT NULL,
  grant_jti text,
  status text NOT NULL CHECK (status IN ('live', 'revoked', 'expired', 'failed', 'test')),
  expires_at timestamptz NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  renewed_at timestamptz,
  ended_at timestamptz,
  reason text
);
CREATE INDEX releases_live ON releases (expires_at) WHERE status = 'live';

-- Append-only audit (§7.10): each row's hash covers the previous row's hash
-- and the row's canonical JSON. Writers may only INSERT; the trigger below
-- (owned by the migration owner) fills in the chain, the time and the
-- database role, ignoring whatever the inserter sent.
CREATE TABLE audit_log (
  seq bigint PRIMARY KEY,
  id text NOT NULL UNIQUE,
  at timestamptz NOT NULL,
  actor jsonb NOT NULL,
  action text NOT NULL,
  resource_type text NOT NULL,
  resource_id text,
  group_id text,
  runner_id text,
  detail jsonb NOT NULL DEFAULT '{}',
  -- The login that wrote the row (session_user), set by the trigger.
  db_role text NOT NULL,
  prev_hash bytea NOT NULL,
  hash bytea NOT NULL
);

-- The chain head: one row, locked FOR UPDATE by each append so the chain
-- never forks. No process role can read or lock it except through the trigger.
CREATE TABLE audit_chain_head (
  one boolean PRIMARY KEY DEFAULT true CHECK (one),
  seq bigint NOT NULL,
  hash bytea NOT NULL
);
INSERT INTO audit_chain_head (seq, hash) VALUES (0, decode(repeat('00', 32), 'hex'));

-- What each row's hash covers: SHA-256(prev_hash || this text, UTF-8).
CREATE FUNCTION audit_canonical(e audit_log) RETURNS text
  LANGUAGE sql STABLE
  SET search_path = pg_catalog, public, pg_temp
AS $$
  SELECT jsonb_build_object(
    'seq', e.seq,
    'id', e.id,
    'at', to_char(e.at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
    'actor', e.actor,
    'action', e.action,
    'resource_type', e.resource_type,
    'resource_id', e.resource_id,
    'group_id', e.group_id,
    'runner_id', e.runner_id,
    'detail', e.detail,
    'db_role', e.db_role
  )::text
$$;

CREATE FUNCTION audit_log_chain() RETURNS trigger
  LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, public, pg_temp
  SET lock_timeout = '2s'
AS $$
DECLARE
  head record;
BEGIN
  SELECT seq, hash INTO STRICT head FROM public.audit_chain_head FOR UPDATE;
  NEW.seq := head.seq + 1;
  NEW.at := clock_timestamp();
  NEW.db_role := session_user::text;
  NEW.prev_hash := head.hash;
  NEW.hash := sha256(head.hash || convert_to(public.audit_canonical(NEW), 'UTF8'));
  UPDATE public.audit_chain_head SET seq = NEW.seq, hash = NEW.hash;
  RETURN NEW;
END
$$;

CREATE TRIGGER audit_log_chain BEFORE INSERT ON audit_log
  FOR EACH ROW EXECUTE FUNCTION audit_log_chain();

-- Grants (§7.9). Nothing is granted to PUBLIC.

REVOKE ALL ON ALL TABLES IN SCHEMA public FROM PUBLIC;
-- Nobody but the owner creates objects in the runner schema (PUBLIC had
-- CREATE on public before Postgres 15).
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO rb_api, rb_scheduler, rb_worker, rb_vault;

-- runner-api: configuration read/write; immutable versions and snapshots
-- insert-only; usage, transcripts and releases read-only (for the UI).
GRANT SELECT, INSERT, UPDATE, DELETE ON groups, runners, providers, profiles, mcp_servers, credentials,
  agents, proposals, policy TO rb_api;
GRANT SELECT, INSERT ON agent_versions, mcp_tool_snapshots TO rb_api;
GRANT SELECT ON releases, usage_events, transcripts, tool_calls, slots, ledger_buckets, ledger_reservations
  TO rb_api;

-- The scheduler (runs in runner-api's process with its own login).
GRANT SELECT, INSERT, UPDATE ON ledger_buckets, ledger_reservations, slots TO rb_scheduler;
GRANT SELECT, INSERT ON usage_events TO rb_scheduler;
GRANT SELECT ON policy, groups, runners, agents, agent_versions TO rb_scheduler;

-- runner-worker: usage, transcripts and tool calls INSERT; agents read.
GRANT INSERT ON usage_events, transcripts, tool_calls TO rb_worker;
GRANT SELECT ON agents, agent_versions TO rb_worker;

-- vault: sole reader and writer of the secret tables; records releases;
-- reads the configuration it checks releases against.
GRANT SELECT, INSERT, UPDATE, DELETE ON credential_secret_versions, ingest_keys, vault_keys, seal_replay
  TO rb_vault;
GRANT SELECT, INSERT, UPDATE ON releases TO rb_vault;
GRANT SELECT ON credentials, agents, agent_versions, groups, runners TO rb_vault;

-- Audit: INSERT only for every role (the trigger extends the chain); only
-- runner-api reads entries (the audit API) and verifies the chain.
GRANT INSERT ON audit_log TO rb_api, rb_scheduler, rb_worker, rb_vault;
GRANT SELECT ON audit_log TO rb_api;
REVOKE ALL ON FUNCTION audit_log_chain() FROM PUBLIC;
REVOKE ALL ON FUNCTION audit_canonical(audit_log) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION audit_canonical(audit_log) TO rb_api;

INSERT INTO policy (id) VALUES (1);
