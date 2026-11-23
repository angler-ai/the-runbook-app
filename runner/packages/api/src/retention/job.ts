import { createHash } from 'node:crypto';
import { promisify } from 'node:util';
import { gzip } from 'node:zlib';
import type pg from 'pg';
import type { RetentionConfig } from './config.ts';
import type { ArchiveStore } from './storage.ts';

const compress = promisify(gzip);
const leaderLock = 7419004;
const dayMs = 86_400_000;

export const sha256 = (bytes: string | Buffer) => createHash('sha256').update(bytes).digest('hex');
export const archiveKey = (org: string, day: string, hash: string) => `${sha256(org)}/${day}-${hash}.jsonl.gz`;

const tables = ['transcripts', 'tool_calls', 'releases', 'usage_events', 'ledger_reservations'] as const;
const timestamps = [
  'created_at', 'created_at', 'GREATEST(created_at, renewed_at, ended_at)', 'created_at', 'GREATEST(created_at, settled_at)',
];
const blocked = ['false', 'false', "status = 'live' AND expires_at >= $1", 'false', "status = 'reserved' OR usage_pending"];
const details = tables.map((table, i) =>
  `SELECT step_run_id, ${timestamps[i]} AS at, ${blocked[i]} AS blocked FROM ${table}
   WHERE ($2::text[] IS NULL OR step_run_id = ANY($2))`,
).join(' UNION ALL ');

interface Snapshot {
  step_run_id: string;
  payload: string;
  transcript_projects: string[];
  tool_owners: { project_id: string; agent_id: string }[];
  last_tool_id: string | null;
}

const snapshot = `
    WITH eligible AS (
      SELECT step_run_id FROM (${details}) detail GROUP BY step_run_id
      HAVING max(at) < $1 AND NOT bool_or(blocked)
    )
    SELECT step_run_id,
      jsonb_build_object('step_run_id', step_run_id, ${tables.map((table) =>
    `'${table}', COALESCE((SELECT jsonb_agg(to_jsonb(t) || jsonb_build_object('id', t.id::text) ORDER BY t.id) FROM ${table} t
      WHERE t.step_run_id = e.step_run_id), '[]'::jsonb)`,
  ).join(', ')})::text AS payload,
      ARRAY(SELECT DISTINCT project_id FROM transcripts WHERE step_run_id = e.step_run_id ORDER BY 1) AS transcript_projects,
      COALESCE((SELECT jsonb_agg(DISTINCT jsonb_build_object('project_id', project_id, 'agent_id', agent_id))
        FROM tool_calls WHERE step_run_id = e.step_run_id), '[]') AS tool_owners,
      (SELECT max(id) FROM tool_calls WHERE step_run_id = e.step_run_id) AS last_tool_id
    FROM eligible e
    WHERE NOT EXISTS (
      SELECT FROM slots s WHERE s.step_run_id = e.step_run_id AND (
        s.released_at IS NULL OR (s.sandbox_started_at IS NOT NULL AND s.sandbox_stopped_at IS NULL)
        OR GREATEST(s.created_at, s.released_at, s.reported_at, s.sandbox_stopped_at, s.metering_closed_at) >= $1
        OR EXISTS (SELECT FROM slots active WHERE active.execution_id = s.execution_id AND active.released_at IS NULL)
      )
    )
  `;

async function prepare(org: string, day: string, rows: Snapshot[]) {
  // Each line is a gzip member. An authorized read can fetch and verify just
  // the member for its step, while standard gunzip extracts the entire JSONL.
  const members = [await compress(JSON.stringify({ format: 1, org_id: org, day, step_count: rows.length }) + '\n')];
  const index = [];
  let offset = members[0]!.length;
  for (const { payload, ...row } of rows) {
    const member = await compress(payload + '\n');
    index.push({ ...row, byte_offset: offset, byte_length: member.length, sha256: sha256(member) });
    members.push(member);
    offset += member.length;
  }
  const bytes = Buffer.concat(members, offset);
  return { bytes, sha: sha256(bytes), index };
}

async function finalize(db: pg.PoolClient, day: string, now: Date, cutoff: Date,
  expected: Map<string, string>, archive: Awaited<ReturnType<typeof prepare>> | undefined) {
  await db.query('BEGIN');
  try {
    if (archive) {
      // A writer may have added or changed detail since the archive was
      // prepared. Hold the write barrier only for revalidation and deletion.
      await db.query("SET LOCAL lock_timeout = '2s'");
      await db.query('SELECT lock_runner_archive_detail()');
      const ids = [...expected.keys()];
      const { rows: current } = await db.query<{ step_run_id: string; sha: string }>(`
        SELECT step_run_id, encode(sha256(convert_to(payload, 'UTF8')), 'hex') AS sha
        FROM (${snapshot}) detail
      `, [cutoff, ids]);
      if (current.length !== expected.size || current.some(({ step_run_id, sha }) => expected.get(step_run_id) !== sha)) {
        throw new Error('runner detail changed during archival');
      }

      await db.query(`
        INSERT INTO runner_archive_steps
          (day, step_run_id, transcript_projects, tool_owners, last_tool_id, byte_offset, byte_length, sha256)
        SELECT $1, step_run_id, transcript_projects, tool_owners, last_tool_id, byte_offset, byte_length, sha256
        FROM jsonb_to_recordset($2::jsonb) AS indexed(step_run_id text, transcript_projects text[],
          tool_owners jsonb, last_tool_id bigint, byte_offset bigint, byte_length bigint, sha256 text)
      `, [day, JSON.stringify(archive.index)]);
      await db.query(`
        INSERT INTO runner_archive_daily_usage (day, step_run_id, execution_id, project_id, agent_id, group_id,
          runner_id, provider, model, kind, cost_usd, input_tokens, output_tokens, cached_tokens)
        SELECT (u.created_at AT TIME ZONE p.time_zone)::date, u.step_run_id, s.execution_id,
          u.project_id, u.agent_id, u.group_id, u.runner_id, u.provider, u.model, u.kind,
          sum(u.cost_usd), sum(u.input_tokens), sum(u.output_tokens), sum(u.cached_tokens)
        FROM usage_events u CROSS JOIN policy p
        LEFT JOIN ledger_reservations r ON r.id = u.reservation_id
        LEFT JOIN slots s ON s.id = r.slot_id
        WHERE u.step_run_id = ANY($1) GROUP BY 1,2,3,4,5,6,7,8,9,10
      `, [ids]);
      // Usage events reference the reservation, so remove them first.
      for (const table of tables) await db.query(`DELETE FROM ${table} WHERE step_run_id = ANY($1)`, [ids]);
    }
    await db.query('UPDATE runner_archive_days SET archived_at = $2 WHERE day = $1', [day, now]);
    await db.query('COMMIT');
  } catch (error) {
    await db.query('ROLLBACK');
    throw error;
  }
}

async function expire(db: pg.PoolClient, store: ArchiveStore, org: string, cutoff: Date) {
  const { rows } = await db.query<{ day: string; sha256: string }>(`
    SELECT day::text, sha256 FROM runner_archive_days
    WHERE archived_at <= $1 AND purged_at IS NULL AND sha256 IS NOT NULL ORDER BY day
  `, [cutoff]);
  for (const { day, sha256: hash } of rows) {
    await store.remove(archiveKey(org, day, hash));
    // Readers already filter by the archive window, even if this cleanup is retried.
    await db.query('DELETE FROM runner_archive_steps WHERE day = $1', [day]);
    await db.query('UPDATE runner_archive_days SET purged_at = now() WHERE day = $1', [day]);
  }
}

async function removeObsolete(db: pg.PoolClient, store: ArchiveStore, org: string) {
  const { rows } = await db.query<{ day: string; obsolete_sha256: string[] }>(`
    SELECT day::text, obsolete_sha256 FROM runner_archive_days
    WHERE archived_at IS NOT NULL AND cardinality(obsolete_sha256) > 0
  `);
  // Delete superseded objects only after the day's final checksum is fixed.
  // The journal is kept until deletion succeeds, including across crashes.
  for (const { day, obsolete_sha256: obsolete } of rows) {
    for (const hash of obsolete) await store.remove(archiveKey(org, day, hash));
    await db.query("UPDATE runner_archive_days SET obsolete_sha256 = '{}' WHERE day = $1", [day]);
  }
}

export async function runRetention(pool: pg.Pool, store: ArchiveStore, org: string, config: RetentionConfig,
  now = new Date()): Promise<number> {
  if (config.liveDays === null) return 0;
  const db = await pool.connect();
  // Release the session lock on disconnect, including an interrupted upload.
  let locked = false;
  let discard = false;
  try {
    const result = await db.query<{ locked: boolean }>('SELECT pg_try_advisory_lock($1) AS locked', [leaderLock]);
    locked = result.rows[0]!.locked;
    if (!locked) return 0;
    await removeObsolete(db, store, org);
    if (config.archiveDays !== null) await expire(db, store, org, new Date(+now - config.archiveDays * dayMs));
    const today = now.toISOString().slice(0, 10);
    const pending = await db.query<{ day: string }>(
      'SELECT day::text FROM runner_archive_days WHERE archived_at IS NULL',
    );
    const day = pending.rows[0]?.day ?? today;
    if (!pending.rowCount && !(await db.query(`
      INSERT INTO runner_archive_days (day) VALUES ($1) ON CONFLICT DO NOTHING
    `, [day])).rowCount) return 0;

    // A crashed day keeps its key and cutoff when retried after midnight.
    const cutoff = new Date(Date.parse(day) - config.liveDays * dayMs);
    await db.query("SET TIME ZONE 'UTC'");
    const { rows } = await db.query<Snapshot>(`${snapshot} ORDER BY step_run_id`, [cutoff, null]);
    const expected = new Map(rows.map(({ step_run_id, payload }) => [step_run_id, sha256(payload)]));
    const archive = rows.length ? await prepare(org, day, rows) : undefined;
    rows.length = 0;
    await db.query(`
      UPDATE runner_archive_days
      SET obsolete_sha256 = array_remove(
        CASE WHEN sha256 IS NOT NULL AND sha256 IS DISTINCT FROM $2::text
          THEN array_append(obsolete_sha256, sha256) ELSE obsolete_sha256 END, $2::text),
        sha256 = $2
      WHERE day = $1
    `, [day, archive?.sha ?? null]);

    if (archive) {
      // Content-addressed keys prevent a disconnected former leader from
      // overwriting a different snapshot committed by the new leader.
      const key = archiveKey(org, day, archive.sha);
      await store.put(key, archive.bytes);
      if (sha256(await store.get(key)) !== archive.sha) throw new Error('runner archive verification failed');
    }
    await finalize(db, day, now, cutoff, expected, archive);
    await removeObsolete(db, store, org);
    return expected.size;
  } catch (error) {
    discard = true;
    throw error;
  } finally {
    try {
      if (locked && !discard) await db.query('SELECT pg_advisory_unlock($1)', [leaderLock]);
    } catch {
      discard = true;
    } finally {
      db.release(discard);
    }
  }
}
