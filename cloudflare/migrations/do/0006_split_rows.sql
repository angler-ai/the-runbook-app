-- ExecutionDO 0006 (CF13): each step run, and each open assignment of a
-- derived row, in a row of its own.
--
-- The head (`meta.projection`) kept every step run's outputs and summary,
-- and each derived row (`derived_outbox.entry`) every open assignment, so a
-- long or wide run passed Durable Object SQLite's 2,000,000-byte row and
-- every later commit failed. From CF13:
-- - the head keeps the run-level fields only (no `steps` key), and each
--   step run is one `step_run` row: `body` its projection as JSON,
--   `position` its place in the run's step list;
-- - a derived row keeps no `assignments` key, and its assignments are rows
--   of `derived_assignment` (its sequence, then each one's place).
-- A head written before CF13 (it has `steps`) is read as it is and split by
-- its run's next commit; a derived row written before CF13 (it has
-- `assignments`) is read as it is. An older release refuses the new shapes
-- (the key it requires is missing) instead of reading a run with no steps.
ALTER TABLE step_run ADD COLUMN position INTEGER;
ALTER TABLE step_run ADD COLUMN body TEXT;
CREATE TABLE derived_assignment (
  org_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  sequence INTEGER NOT NULL CHECK (sequence >= 0),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  body TEXT NOT NULL,
  PRIMARY KEY (sequence, ordinal)
);
