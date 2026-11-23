import type { Label } from './core.ts';

export interface Tool {
  name: string;
  description?: string;
  inputSchema?: Record<string, unknown>;
}

export interface ToolSnapshot {
  snapshot_id: string;
  hash: string;
  tools: Tool[];
}

export interface PinnedTools {
  snapshot: ToolSnapshot | null;
}

export interface ToolDrift {
  latest: ToolSnapshot | null;
  changes: {
    added: string[];
    // Current tool definitions are in latest.tools.
    changed: { name: string; before: Tool; schema_changed: boolean }[];
    removed: Tool[];
  };
}

export interface ToolConnection {
  id: string;
  name: string;
  pinned_snapshot_id: string | null;
  tools: { name: string; label: Label }[];
}

export interface PinToolsInput {
  snapshot_id: string;
  tool_labels?: Record<string, Label>;
  never_allowed?: string[];
}
