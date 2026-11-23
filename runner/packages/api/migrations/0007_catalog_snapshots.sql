ALTER TABLE mcp_servers ADD COLUMN deleted_at timestamptz;
ALTER TABLE mcp_servers DROP CONSTRAINT mcp_servers_name_key;
CREATE UNIQUE INDEX mcp_servers_name ON mcp_servers (name) WHERE deleted_at IS NULL;
ALTER TABLE mcp_tool_snapshots ADD COLUMN source_hash bytea;
