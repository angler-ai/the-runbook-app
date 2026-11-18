-- B3.3: search projection columns beside the exact run_summary payload.
-- fields holds plain JSON of indexed custom fields only (search/index
-- semantics); the exact text body remains authoritative.
ALTER TABLE runbook.run_summary
    ADD COLUMN title text NOT NULL DEFAULT '',
    ADD COLUMN tags text[] NOT NULL DEFAULT '{}',
    ADD COLUMN fields jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN created_by text NOT NULL DEFAULT '',
    ADD COLUMN created_at_ms bigint NOT NULL DEFAULT 0,
    ADD COLUMN search_tsv tsvector;
CREATE INDEX run_summary_search_tsv ON runbook.run_summary USING GIN (search_tsv);
CREATE INDEX run_summary_fields ON runbook.run_summary USING GIN (fields);
CREATE INDEX run_summary_tags ON runbook.run_summary USING GIN (tags);
CREATE INDEX run_summary_created
    ON runbook.run_summary (created_at_ms DESC, execution_id DESC);
