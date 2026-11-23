import type pg from 'pg';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';

import { archivedTranscripts, type ArchiveReader, type TranscriptRow } from '../retention/read.ts';

type TranscriptPrincipal = Pick<UserAssertionClaims, 'scopes' | 'project_roles'>;

export function transcriptAllowed(claims: TranscriptPrincipal, projectId: string): boolean {
  return claims.scopes.includes('runner:read') && Object.hasOwn(claims.project_roles, projectId);
}

export async function transcriptPage(pool: pg.Pool, claims: TranscriptPrincipal, stepId: string,
  after = 0, claimId?: string, reader?: ArchiveReader) {
  if (!claims.scopes.includes('runner:read')) return { items: [] };
  const projects = Object.keys(claims.project_roles);
  const { rows } = await pool.query<TranscriptRow>(
    `SELECT id, step_run_id, project_id, group_id, runner_id, agent_id, claim_id, generation::double precision AS generation, seq, entry, created_at
     FROM transcripts WHERE step_run_id = $1 AND project_id = ANY($2::text[]) AND seq > $3
       AND ($4::text IS NULL OR claim_id = $4)
     ORDER BY seq, id LIMIT 1000`,
    [stepId, projects, after, claimId ?? null],
  );
  // Hot rows first so a concurrent archive commit can only yield overlap.
  const archived = reader && projects.length ? await archivedTranscripts(pool, reader, stepId, projects, after, claimId) : [];
  const byId = new Map(rows.map((row) => [row.id, row as TranscriptRow & { archived?: true; archived_at?: string }]));
  for (const row of archived) byId.set(row.id, row);
  const items = [...byId.values()].sort((a, b) => a.seq - b.seq ||
    (a.id === b.id ? 0 : BigInt(a.id) < BigInt(b.id) ? -1 : 1))
    .slice(0, 1000).map(({ id: _id, ...row }) => row);
  const date = items.find((row) => row.archived)?.archived_at;
  return { items, ...(date && { archived: true, archived_at: date }) };
}

export async function readTranscript(pool: pg.Pool, claims: TranscriptPrincipal, stepId: string, after = 0, claimId?: string) {
  return (await transcriptPage(pool, claims, stepId, after, claimId)).items;
}
