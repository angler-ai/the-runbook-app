-- ExecutionDO 0001: one execution's SQLite storage (CF2, D56).
--
-- Authoritative for the Durable Object. It is applied inside the object on
-- first use by `runbook-store-do` (crates/runbook-store-do/src/migrate.rs),
-- which records it in `do_migration` with its SHA-256 and never re-applies
-- or edits it: a change is a new numbered file. D1's tables live in
-- `cloudflare/migrations/d1/` (CF3a); nothing here is cross-execution.
--
-- Every run-owned row carries org_id, project_id and execution_id (PRD §14
-- Rules), so a run exports, archives or deletes as a unit and the rows map
-- one to one onto the Postgres tables. JSON bodies are TEXT, hashes BLOB,
-- times epoch-millisecond INTEGER, booleans INTEGER 0/1.

-- The head: one row. revision is the last committed event sequence.
-- (Postgres: runbook.execution)
CREATE TABLE meta (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 0),
  runbook_id TEXT NOT NULL,
  version_id TEXT NOT NULL,
  status TEXT NOT NULL,
  completed_at_ms INTEGER,
  projection TEXT NOT NULL
);

-- The event log, authoritative for replay. (runbook.execution_event)
CREATE TABLE event (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  seq INTEGER PRIMARY KEY CHECK (seq > 0),
  at_ms INTEGER NOT NULL,
  type TEXT NOT NULL,
  envelope TEXT NOT NULL
);

-- Step runs, a read index of the projection rewritten on every commit.
CREATE TABLE step_run (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  step_run_id TEXT PRIMARY KEY,
  path TEXT NOT NULL,
  status TEXT NOT NULL,
  verdict TEXT,
  summary TEXT
);

-- Live timers and their generation history. (runbook.execution_timer,
-- runbook.execution_timer_history) The earliest due one sets the alarm.
CREATE TABLE timer (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  key TEXT PRIMARY KEY,
  generation INTEGER NOT NULL CHECK (generation > 0),
  due_at_ms INTEGER NOT NULL
);
CREATE INDEX timer_due ON timer (due_at_ms);
CREATE TABLE timer_history (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  key TEXT PRIMARY KEY,
  generation INTEGER NOT NULL CHECK (generation > 0),
  due_at_ms INTEGER NOT NULL
);

-- Notification outbox: one intent per event ID, drained in sequence order
-- (sequence 0 is the start's `execution.queued`). acked_at_ms marks it
-- delivered, by the drain cursor or a work-queue ack. (runbook.execution_outbox)
CREATE TABLE outbox (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  event_id TEXT PRIMARY KEY,
  sequence INTEGER NOT NULL CHECK (sequence >= 0),
  at_ms INTEGER NOT NULL,
  kind TEXT NOT NULL,
  intent TEXT NOT NULL,
  acked_at_ms INTEGER
);
CREATE INDEX outbox_pending ON outbox (sequence, event_id) WHERE acked_at_ms IS NULL;

-- Derived-row outbox for D1 (runs list, inbox, admission release, memory
-- proposals): one DerivedUpdate per sequence, applied by D1 idempotently by
-- (execution, sequence). (runbook.execution_derived_outbox)
CREATE TABLE derived_outbox (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  sequence INTEGER PRIMARY KEY CHECK (sequence >= 0),
  at_ms INTEGER NOT NULL,
  entry TEXT NOT NULL,
  acked_at_ms INTEGER
);

-- Delivery backoff per outbox stream, advanced when a delivery attempt
-- fails; cleared by any ack.
CREATE TABLE outbox_retry (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  stream TEXT PRIMARY KEY CHECK (stream IN ('notification', 'derived')),
  attempts INTEGER NOT NULL CHECK (attempts > 0),
  next_attempt_at_ms INTEGER NOT NULL
);

-- Request idempotency for this execution. (runbook.execution_idempotency)
CREATE TABLE idempotency (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  key TEXT PRIMARY KEY,
  request_hash BLOB NOT NULL CHECK (length(request_hash) = 32),
  body TEXT NOT NULL
);

-- Actor replay records, kept 24 hours. (runbook.actor_replay)
CREATE TABLE actor_replay (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  prefix TEXT NOT NULL,
  at_ms INTEGER NOT NULL,
  body TEXT NOT NULL,
  PRIMARY KEY (prefix, at_ms)
);
CREATE INDEX actor_replay_expiry ON actor_replay (at_ms);

-- Claims and leases, fenced by generation. (runbook.execution_claim)
CREATE TABLE claim (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  step_run_id TEXT PRIMARY KEY,
  claim_id TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK (generation > 0),
  released INTEGER NOT NULL CHECK (released IN (0, 1)),
  lease_deadline_at_ms INTEGER,
  body TEXT NOT NULL
);

-- Appended step log lines, numbered from 1. CF4 moves them to R2 chunks
-- with a short inline tail (PRD §14). (runbook.claim_log)
CREATE TABLE claim_log (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  step_run_id TEXT NOT NULL,
  seq INTEGER NOT NULL CHECK (seq > 0),
  line TEXT NOT NULL,
  PRIMARY KEY (step_run_id, seq)
);

-- Pinned start facts (one row) and the activation time D24 starts from:
-- the admission time, or NULL while the run is queued in D1.
-- (runbook.execution_start)
CREATE TABLE start_record (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  body TEXT NOT NULL,
  activation_at_ms INTEGER
);

-- Work-queue leases for timers, notifications, activation and claim
-- deadlines; generation fences acknowledgements. (runbook.work_lease)
CREATE TABLE work_lease (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('timer', 'notification', 'activation', 'claim')),
  work_key TEXT NOT NULL,
  work_generation INTEGER NOT NULL CHECK (work_generation >= 0),
  generation INTEGER NOT NULL CHECK (generation > 0),
  deadline_at_ms INTEGER NOT NULL,
  acknowledged INTEGER NOT NULL CHECK (acknowledged IN (0, 1)),
  PRIMARY KEY (kind, work_key, work_generation)
);

-- Execution credential families and tokens: hashes only, never secrets.
-- (runbook.credential_family, runbook.execution_credential)
CREATE TABLE credential_family (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  family_id TEXT PRIMARY KEY,
  idempotency_key TEXT NOT NULL UNIQUE,
  body TEXT NOT NULL
);
CREATE TABLE execution_credential (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  token_id TEXT PRIMARY KEY,
  hash BLOB NOT NULL UNIQUE CHECK (length(hash) = 32),
  family_id TEXT NOT NULL REFERENCES credential_family (family_id),
  body TEXT NOT NULL
);
CREATE INDEX execution_credential_family ON execution_credential (family_id);

-- Host diagnostics: what each alarm did. Not run data, not exported.
CREATE TABLE alarm_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  fired_at_ms INTEGER NOT NULL,
  outcome TEXT NOT NULL
);
