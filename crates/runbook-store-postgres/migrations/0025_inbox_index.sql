-- R27: `/v1/me/tasks` is one keyset query over open assignments instead of
-- loading every project's assignments into memory. Scalar copies of the
-- inbox facts sit beside the exact serde body (still authoritative):
-- recipients as 'user:<id>' / 'team:<id>', voters as user ids.
ALTER TABLE runbook.assignment
    ADD COLUMN kind text NOT NULL DEFAULT 'work',
    ADD COLUMN updated_at_ms bigint NOT NULL DEFAULT 0,
    ADD COLUMN recipients text[] NOT NULL DEFAULT '{}',
    ADD COLUMN voted_by text[] NOT NULL DEFAULT '{}';
UPDATE runbook.assignment SET
    kind = COALESCE(body::jsonb ->> 'kind', 'work'),
    updated_at_ms = COALESCE((body::jsonb ->> 'updated_at_ms')::bigint, 0),
    recipients = ARRAY(
        SELECT (r ->> 'kind') || ':' || (r ->> 'reference')
        FROM jsonb_array_elements(COALESCE(body::jsonb -> 'recipients', '[]'::jsonb)) r
    ),
    voted_by = ARRAY(
        SELECT jsonb_array_elements_text(COALESCE(body::jsonb -> 'voted_by', '[]'::jsonb))
    );
-- Recipient overlap narrows to the caller's rows; the keyset order index
-- (byte order, matching the Rust cursor) serves newest-first pages.
CREATE INDEX assignment_recipients ON runbook.assignment USING GIN (recipients);
CREATE INDEX assignment_inbox_order ON runbook.assignment
    (updated_at_ms DESC, execution_id COLLATE "C" DESC, step_run_id COLLATE "C" DESC);
