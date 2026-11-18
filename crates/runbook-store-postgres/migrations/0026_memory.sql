-- MEM2 (PRD §7 "Runbook memory", §14; D40–D42): curated runbook memory.
-- Every row carries org_id and project_id. `body` is the exact encoded
-- record (already redacted by runbook-core); the other columns index it.
-- `scope_key` is the runbook ID for a runbook entry and 'project' for a
-- project entry. `search_tsv` holds the core's normalized words of title
-- and body ('simple' configuration: lowercase, no stemming).
CREATE TABLE runbook.memory_entry (
    memory_id text PRIMARY KEY,
    org_id text NOT NULL,
    project_id text NOT NULL,
    scope_key text NOT NULL,
    status text NOT NULL CHECK (status IN ('proposed', 'active', 'archived')),
    step_id text,
    tags text[] NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    expires_at_ms bigint,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    -- D40: the execution and event sequence that proposed the entry.
    source_execution_id text,
    proposal_sequence bigint,
    body text NOT NULL,
    search_tsv tsvector NOT NULL,
    CHECK ((source_execution_id IS NULL) = (proposal_sequence IS NULL))
);
CREATE INDEX memory_entry_quota
    ON runbook.memory_entry (org_id, project_id, scope_key, status);
CREATE INDEX memory_entry_recent
    ON runbook.memory_entry (org_id, project_id, updated_at_ms DESC, memory_id COLLATE "C" DESC);
CREATE INDEX memory_entry_step
    ON runbook.memory_entry (org_id, project_id, step_id) WHERE status = 'active';
CREATE INDEX memory_entry_search ON runbook.memory_entry USING GIN (search_tsv);
CREATE INDEX memory_entry_tags ON runbook.memory_entry USING GIN (tags);
CREATE INDEX memory_entry_proposal
    ON runbook.memory_entry (source_execution_id, proposal_sequence)
    WHERE source_execution_id IS NOT NULL;

-- Every revision, immutable (PRD §7 "edits keep prior revisions").
CREATE TABLE runbook.memory_revision (
    memory_id text NOT NULL REFERENCES runbook.memory_entry (memory_id),
    revision bigint NOT NULL CHECK (revision > 0),
    org_id text NOT NULL,
    project_id text NOT NULL,
    action text NOT NULL,
    at_ms bigint NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (memory_id, revision)
);
CREATE INDEX memory_revision_tenant ON runbook.memory_revision (org_id, project_id);
