-- D23 permits arbitrary UTF-8, including NUL, in unpublished drafts.
-- Existing TEXT source is converted losslessly; published valid source is unchanged.
ALTER TABLE runbook.runbook
    ALTER COLUMN draft_source TYPE BYTEA USING convert_to(draft_source, 'UTF8');
