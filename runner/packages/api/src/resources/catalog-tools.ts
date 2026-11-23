import { isDeepStrictEqual } from 'node:util';
import type pg from 'pg';
import type { PinnedTools, ToolConnection, ToolDrift, ToolSnapshot } from '@runbook/runner-shared/contracts/catalog';
import type { Label, UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { HttpError } from '@runbook/runner-shared/http';
import { authorize } from '../authorize.ts';

const snapshotColumns = "id AS snapshot_id, encode(hash, 'hex') AS hash, tools";
const latestFirst = 'ORDER BY pinned_at DESC, id DESC LIMIT 1';

function toolDefinitions(snapshot: ToolSnapshot | undefined): ToolSnapshot | null {
  if (!snapshot) return null;
  return {
    ...snapshot,
    tools: snapshot.tools.map(({ name, description, inputSchema }) => ({
      name, ...(typeof description === 'string' && { description }), inputSchema,
    })),
  };
}

export async function pinnedTools(pool: pg.Pool, serverId: string, pin: unknown): Promise<PinnedTools> {
  const { rows: [snapshot] } = await pool.query<ToolSnapshot>(
    `SELECT ${snapshotColumns} FROM mcp_tool_snapshots WHERE server_id = $1 AND id = $2`, [serverId, pin],
  );
  return { snapshot: toolDefinitions(snapshot) };
}

export async function toolDrift(pool: pg.Pool, serverId: string, pin: unknown, sourceHash: Buffer): Promise<ToolDrift> {
  const [{ snapshot }, { rows: [discovered] }] = await Promise.all([
    pinnedTools(pool, serverId, pin),
    pool.query<ToolSnapshot>(
      `SELECT ${snapshotColumns} FROM mcp_tool_snapshots WHERE server_id = $1 AND source_hash = $2 ${latestFirst}`,
      [serverId, sourceHash],
    ),
  ]);
  const latest = toolDefinitions(discovered);
  const changes: ToolDrift['changes'] = { added: [], changed: [], removed: [] };
  if (!latest || latest.hash === snapshot?.hash) return { latest, changes };

  const previous = new Map(snapshot?.tools.map((tool) => [tool.name, tool]));
  for (const after of latest.tools) {
    const before = previous.get(after.name);
    if (!before) {
      changes.added.push(after.name);
      continue;
    }
    const schema_changed = !isDeepStrictEqual(before.inputSchema, after.inputSchema);
    if (schema_changed || before.description !== after.description) {
      changes.changed.push({ name: after.name, before, schema_changed });
    }
    previous.delete(after.name);
  }
  changes.removed = [...previous.values()];
  return { latest, changes };
}

// Compare the same fields as toolDrift, using JSONB equality so schema keys
// and tool order do not cause a warning. The list does not return schemas.
function comparableTools(column: string) {
  return `(SELECT jsonb_object_agg(tool->>'name', jsonb_build_array(
       CASE WHEN jsonb_typeof(tool->'description') = 'string' THEN tool->'description' END, tool->'inputSchema'))
     FROM jsonb_array_elements(${column}) tool)`;
}

export async function toolSummaries(pool: pg.Pool, servers: { id: string; sourceHash: Buffer }[]) {
  const { rows } = await pool.query<{ id: string; pinned_tool_count: number; drift: boolean }>(
    `SELECT m.id, coalesce(jsonb_array_length(pin.tools), 0) AS pinned_tool_count,
       CASE WHEN latest.hash <> pin.hash
         THEN ${comparableTools('pin.tools')} IS DISTINCT FROM ${comparableTools('latest.tools')}
         ELSE false END AS drift
     FROM unnest($1::text[], $2::bytea[]) current_source(id, source_hash)
     JOIN mcp_servers m ON m.id = current_source.id
     LEFT JOIN mcp_tool_snapshots pin ON pin.id = m.pinned_snapshot_id AND pin.server_id = m.id
     LEFT JOIN LATERAL (
       SELECT hash, tools FROM mcp_tool_snapshots
       WHERE pin.id IS NOT NULL AND server_id = m.id AND source_hash = current_source.source_hash ${latestFirst}
     ) latest ON true`, [servers.map(({ id }) => id), servers.map(({ sourceHash }) => sourceHash)],
  );
  return new Map(rows.map(({ id, ...summary }) => [id, summary]));
}

export async function projectToolConnections(pool: pg.Pool, claims: UserAssertionClaims, projectId: string) {
  if (authorize(claims, 'read', { kind: 'agent', projectId }) !== 'allow') {
    throw new HttpError(403, 'auth.denied', 'runner resource unavailable');
  }
  const { rows } = await pool.query<Omit<ToolConnection, 'tools'> & {
    tool_names: string[]; tool_labels: Record<string, Label>; never_allowed: string[];
  }>(
    `SELECT m.id, m.name, m.pinned_snapshot_id,
       coalesce(jsonb_path_query_array(s.tools, '$[*].name'), '[]') AS tool_names, m.tool_labels, m.never_allowed
     FROM mcp_servers m
     LEFT JOIN mcp_tool_snapshots s ON s.id = m.pinned_snapshot_id AND s.server_id = m.id
     WHERE m.deleted_at IS NULL AND (m.sharing->>'projects' = 'all' OR (m.sharing->'projects') @> jsonb_build_array($1::text))
     ORDER BY m.name LIMIT 1000`, [projectId],
  );
  const items: ToolConnection[] = rows.map(({ tool_names, tool_labels, never_allowed, ...row }) => {
    const blocked = new Set(never_allowed);
    return {
      ...row,
      tools: tool_names.filter((name) => !blocked.has(name)).map((name) => ({
        name, label: Object.hasOwn(tool_labels, name) ? tool_labels[name]! : 'destructive',
      })),
    };
  });
  return { items };
}
