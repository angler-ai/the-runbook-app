-- ExecutionDO 0005 (gate round 2): each hold names the import that made it.
-- A held run wakes one hour after `held_at_ms` (to expire if its import
-- never committed), and a later import may replace a hold older than ten
-- minutes once its own D1 guards passed (crates/runbook-store-do/src/portable.rs).
ALTER TABLE import_hold ADD COLUMN import_id TEXT NOT NULL DEFAULT '';
