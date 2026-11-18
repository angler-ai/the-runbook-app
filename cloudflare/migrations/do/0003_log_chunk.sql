-- ExecutionDO 0003 (CF5, CF4-8): step log chunks in R2.
--
-- `append_logs` keeps writing lines to `claim_log` inside the object's
-- transaction. After it, the host moves each full chunk (the oldest
-- unflushed lines of a step run, up to 64 KiB) to R2 under the run's prefix
-- (`logs/{step_run}/{chunk:06}`), then, in a second transaction, records
-- the chunk here and deletes its lines from `claim_log`. Reads stitch the
-- chunks and the inline tail. Sequence numbers and the per-step 5 MiB cap
-- count both, so neither changes.
CREATE TABLE log_chunk (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  step_run_id TEXT NOT NULL,
  chunk INTEGER NOT NULL CHECK (chunk >= 0),
  first_seq INTEGER NOT NULL CHECK (first_seq > 0),
  last_seq INTEGER NOT NULL CHECK (last_seq >= first_seq),
  bytes INTEGER NOT NULL CHECK (bytes >= 0),
  PRIMARY KEY (step_run_id, chunk)
);
