ALTER TABLE usage_events ADD COLUMN cache_write_tokens bigint NOT NULL DEFAULT 0 CHECK (cache_write_tokens >= 0);
