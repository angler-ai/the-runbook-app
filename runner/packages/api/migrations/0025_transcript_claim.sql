ALTER TABLE transcripts ADD COLUMN claim_id text;
ALTER TABLE transcripts ADD COLUMN generation bigint;
CREATE INDEX transcripts_claim_seq ON transcripts (step_run_id, claim_id, seq);
