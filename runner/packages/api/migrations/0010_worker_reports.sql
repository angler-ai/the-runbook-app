CREATE SEQUENCE transcript_order_seq AS integer;
SELECT setval('transcript_order_seq', COALESCE(MAX(seq), 0) + 1, false) FROM transcripts;
ALTER TABLE transcripts ALTER COLUMN seq SET DEFAULT nextval('transcript_order_seq');
GRANT USAGE ON SEQUENCE transcript_order_seq TO rb_worker;

ALTER TABLE slots
  ADD COLUMN metering_closed_at timestamptz,
  ADD COLUMN sandbox_id text;
CREATE INDEX slots_live_sandbox ON slots(sandbox_id) WHERE released_at IS NULL;
