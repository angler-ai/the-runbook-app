import { promisify } from 'node:util';
import { gunzip } from 'node:zlib';
import type pg from 'pg';
import { archiveKey, sha256 } from './job.ts';
import type { ArchiveStore } from './storage.ts';

const decompress = promisify(gunzip);

export interface ArchiveReader {
  store: ArchiveStore;
  orgId: string;
  archiveDays: number | null;
}

export interface TranscriptRow {
  id: string;
  step_run_id: string;
  project_id: string;
  group_id: string;
  runner_id: string;
  agent_id: string;
  claim_id: string | null;
  generation: number | null;
  seq: number;
  entry: unknown;
  created_at: string | Date;
}

interface ToolCall {
  id: string;
  agent_id: string;
  project_id: string;
  created_at: string | Date;
}

interface ArchivedStep {
  transcripts: TranscriptRow[];
  tool_calls: ToolCall[];
}

interface IndexRow {
  day: string;
  archived_at: string;
  file_sha: string;
  sha256: string;
  byte_offset: string;
  byte_length: string;
  last_tool_id: string | null;
}

const indexQuery = `
  SELECT d.day::text, (d.archived_at AT TIME ZONE 'UTC')::date::text AS archived_at, d.sha256 AS file_sha,
    s.sha256, byte_offset, byte_length, last_tool_id
  FROM runner_archive_steps s JOIN runner_archive_days d USING (day)
  WHERE d.purged_at IS NULL AND ($1::int IS NULL OR d.archived_at > now() - $1 * interval '24 hours')
`;

async function loadStep(reader: ArchiveReader, row: IndexRow) {
  const bytes = await reader.store.get(archiveKey(reader.orgId, row.day, row.file_sha),
    { offset: Number(row.byte_offset), length: Number(row.byte_length) });
  if (sha256(bytes) !== row.sha256) throw new Error('runner archive step verification failed');
  return JSON.parse((await decompress(bytes)).toString()) as ArchivedStep;
}

function isoTime(value: string | Date) {
  return new Date(value).toISOString();
}

export async function archivedTranscripts(pool: pg.Pool, reader: ArchiveReader, step: string,
  projects: string[], after: number, claim: string | undefined) {
  const { rows } = await pool.query<IndexRow>(`${indexQuery}
    AND step_run_id = $2 AND transcript_projects && $3::text[] ORDER BY day DESC`,
  [reader.archiveDays, step, projects]);
  const allowed = new Set(projects);
  const items = [];
  for (const row of rows) {
    const detail = await loadStep(reader, row);
    for (const { id, step_run_id, project_id, group_id, runner_id, agent_id, claim_id, generation, seq, entry, created_at } of detail.transcripts) {
      if (!allowed.has(project_id) || seq <= after || (claim !== undefined && claim_id !== claim)) continue;
      items.push({ id, step_run_id, project_id, group_id, runner_id, agent_id, claim_id, generation, seq, entry,
        created_at: isoTime(created_at), archived: true as const, archived_at: row.archived_at });
    }
  }
  return items;
}

const limit = 1000;

export async function readToolCalls(pool: pg.Pool, project: string, agent: string, step: string | null, reader?: ArchiveReader) {
  // Read hot detail first. If the job commits between the two reads, the
  // archive copy replaces the hot copy with the same row identity.
  const { rows } = await pool.query<ToolCall>(
    `SELECT * FROM tool_calls WHERE agent_id = $1 AND project_id = $2 AND ($3::text IS NULL OR step_run_id = $3)
     ORDER BY id DESC LIMIT ${limit}`, [agent, project, step],
  );
  if (!reader) return { items: rows };
  const { rows: candidates } = await pool.query<IndexRow>(`${indexQuery}
    AND tool_owners @> $2::jsonb AND ($3::text IS NULL OR step_run_id = $3)
    ORDER BY last_tool_id DESC LIMIT ${limit}`, [reader.archiveDays, JSON.stringify([{ project_id: project, agent_id: agent }]), step]);

  const sort = (a: ToolCall, b: ToolCall) => a.id === b.id ? 0 : BigInt(a.id) > BigInt(b.id) ? -1 : 1;
  type Item = ToolCall & { archived?: true; archived_at?: string };
  let items: Item[] = rows;
  for (const row of candidates) {
    if (!row.last_tool_id || (items.length >= limit && BigInt(row.last_tool_id) < BigInt(items[limit - 1]!.id))) break;
    const detail = await loadStep(reader, row);
    const archived: Item[] = detail.tool_calls.filter((call) => call.project_id === project && call.agent_id === agent)
      .map((call) => ({ ...call, created_at: isoTime(call.created_at), archived: true, archived_at: row.archived_at }));
    const replaced = new Set(archived.map((call) => call.id));
    items = [...items.filter((call) => !replaced.has(call.id)), ...archived].sort(sort).slice(0, limit);
  }
  const date = items.find((call) => call.archived)?.archived_at;
  return { items, ...(date && { archived: true, archived_at: date }) };
}
