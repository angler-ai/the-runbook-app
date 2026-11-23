REVOKE INSERT ON transcripts, usage_events FROM rb_worker;
REVOKE USAGE ON SEQUENCE transcript_order_seq FROM rb_worker;
GRANT INSERT ON transcripts TO rb_api;
GRANT USAGE ON SEQUENCE transcript_order_seq TO rb_api;
