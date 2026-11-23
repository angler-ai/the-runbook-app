CREATE INDEX mcp_tool_snapshots_latest
  ON mcp_tool_snapshots (server_id, source_hash, pinned_at DESC, id DESC);
