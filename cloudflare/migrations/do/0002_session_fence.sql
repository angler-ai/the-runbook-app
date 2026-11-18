-- ExecutionDO 0002: the session write fence (CF2 review fix 2).
--
-- `writes` counts every write that changes state a session reads: each
-- commit (also one that keeps the revision, such as a claim release with
-- no event), heartbeats, the start and its admission. A session records it
-- at begin and its commit is refused as a conflict when it moved, as
-- Postgres's row lock held from begin to commit would have prevented.
ALTER TABLE meta ADD COLUMN writes INTEGER NOT NULL DEFAULT 0 CHECK (writes >= 0);
