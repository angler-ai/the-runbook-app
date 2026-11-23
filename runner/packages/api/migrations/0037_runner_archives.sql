CREATE TABLE runner_archive_days (
  day date PRIMARY KEY,
  archived_at timestamptz,
  sha256 text,
  obsolete_sha256 text[] NOT NULL DEFAULT '{}',
  purged_at timestamptz
);
CREATE UNIQUE INDEX runner_archive_one_pending_day ON runner_archive_days ((true)) WHERE archived_at IS NULL;
CREATE INDEX runner_archive_expiry ON runner_archive_days (archived_at) WHERE purged_at IS NULL AND sha256 IS NOT NULL;

CREATE TABLE runner_archive_steps (
  day date NOT NULL REFERENCES runner_archive_days (day),
  step_run_id text NOT NULL,
  transcript_projects text[] NOT NULL,
  tool_owners jsonb NOT NULL,
  last_tool_id bigint,
  byte_offset bigint NOT NULL,
  byte_length bigint NOT NULL,
  sha256 text NOT NULL,
  PRIMARY KEY (step_run_id, day)
);
CREATE INDEX runner_archive_tools ON runner_archive_steps USING gin (tool_owners);
CREATE INDEX runner_archive_steps_day ON runner_archive_steps (day);

-- Keep aggregate usage after the detail expires. The budget ledger buckets,
-- including closed caps, remain authoritative and are never pruned here.
CREATE TABLE runner_archive_daily_usage (
  day date NOT NULL,
  step_run_id text NOT NULL,
  execution_id text,
  project_id text NOT NULL,
  agent_id text NOT NULL,
  group_id text NOT NULL,
  runner_id text NOT NULL,
  provider text,
  model text,
  kind text NOT NULL,
  cost_usd numeric NOT NULL,
  input_tokens numeric NOT NULL,
  output_tokens numeric NOT NULL,
  cached_tokens numeric NOT NULL
);
CREATE INDEX runner_archive_usage_day ON runner_archive_daily_usage (day);
CREATE INDEX runner_archive_usage_budget ON runner_archive_daily_usage (project_id, day);

CREATE INDEX runner_archive_tools_step ON tool_calls (step_run_id);
CREATE INDEX runner_archive_usage_step ON usage_events (step_run_id);
CREATE INDEX runner_archive_releases_step ON releases (step_run_id);
CREATE INDEX runner_archive_reservations_step ON ledger_reservations (step_run_id);
CREATE INDEX runner_archive_slots_step ON slots (step_run_id);
CREATE INDEX runner_archive_slots_admission ON slots (admission_reservation_id) WHERE admission_reservation_id IS NOT NULL;
CREATE INDEX runner_archive_slots_execution ON slots (execution_id) WHERE released_at IS NULL;

-- Released slots are still used for activity and version history.
ALTER TABLE slots DROP CONSTRAINT slots_admission_reservation_id_fkey;
ALTER TABLE slots ADD FOREIGN KEY (admission_reservation_id) REFERENCES ledger_reservations (id) ON DELETE SET NULL;

GRANT SELECT, INSERT, UPDATE ON runner_archive_days TO rb_api;
GRANT SELECT, INSERT, DELETE ON runner_archive_steps TO rb_api;
GRANT SELECT, INSERT ON runner_archive_daily_usage TO rb_api;
GRANT DELETE ON transcripts, tool_calls, releases, usage_events, ledger_reservations TO rb_api;

-- Serialize only the short verification/deletion transaction against detail
-- writers, including inserts for a previously unseen attempt. The API has
-- read-only access to slots and doesn't need any additional write grants.
CREATE FUNCTION lock_runner_archive_detail() RETURNS void
  LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp
AS $$
  LOCK TABLE public.slots, public.ledger_reservations, public.usage_events,
    public.releases, public.tool_calls, public.transcripts IN SHARE ROW EXCLUSIVE MODE;
$$;
REVOKE ALL ON FUNCTION lock_runner_archive_detail() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION lock_runner_archive_detail() TO rb_api;
