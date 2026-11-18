-- Run-owned actor target index and bounded replay storage (R22/R23).
CREATE TABLE runbook.step_run_index (
    step_run_id text PRIMARY KEY,
    execution_id text NOT NULL REFERENCES runbook.execution ON DELETE CASCADE,
    org_id text NOT NULL,
    project_id text NOT NULL
);
CREATE INDEX step_run_index_execution ON runbook.step_run_index (execution_id);
INSERT INTO runbook.step_run_index (step_run_id, execution_id, org_id, project_id)
SELECT s->>'step_run_id', e.execution_id, o.org_id, o.project_id
FROM runbook.execution e JOIN runbook.execution_owner o USING (execution_id),
LATERAL json_array_elements(e.projection::json->'steps') s;

CREATE TABLE runbook.actor_replay (
    execution_id text NOT NULL REFERENCES runbook.execution ON DELETE CASCADE,
    prefix text NOT NULL,
    at_ms bigint NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (execution_id, prefix, at_ms)
);
CREATE INDEX actor_replay_expiry ON runbook.actor_replay (execution_id, at_ms);
-- Preserve any local pre-migration records; fresh instances have none.
INSERT INTO runbook.actor_replay (execution_id, prefix, at_ms, body)
SELECT execution_id, regexp_replace(key, ':[^:]+$', ':'),
       (body::json->'response'->>'at_ms')::bigint, body
FROM runbook.execution_idempotency WHERE key LIKE 'actor:%';
DELETE FROM runbook.execution_idempotency WHERE key LIKE 'actor:%';
