-- UI-1c: the runbook list reads a tenant's runbooks newest ID first in byte
-- order (`ORDER BY runbook_id COLLATE "C" DESC`, keyset `runbook_id <
-- $3 COLLATE "C"`; crates/runbook-store-postgres/src/authoring.rs,
-- RUNBOOK_LISTING_CHUNK_SQL), the order the reference compares IDs in.
-- Neither the primary key (default collation) nor the slug index has that
-- order, so each 32-row chunk of a search sorted the project's runbooks.
-- Read backwards, this index gives the chunk's rows in order.
CREATE INDEX runbook_listing_order
    ON runbook.runbook (org_id, project_id, runbook_id COLLATE "C");
