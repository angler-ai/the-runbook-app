-- The control D1 database fences concurrent ExecutionDO outbox drains.
-- A NULL sequence is a deletion fence and prevents delayed replays.
CREATE TABLE runner_ready_heads (
    execution_id TEXT NOT NULL PRIMARY KEY,
    sequence INTEGER
);
