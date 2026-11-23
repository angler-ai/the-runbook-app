ALTER TABLE tool_calls ADD COLUMN args_bytes integer CHECK (args_bytes >= 0);
REVOKE INSERT ON tool_calls FROM rb_worker;
GRANT INSERT ON tool_calls TO rb_api;
