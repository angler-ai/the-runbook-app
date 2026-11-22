import type pg from 'pg';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';

type TranscriptPrincipal = Pick<UserAssertionClaims, 'scopes' | 'project_roles'>;

export function transcriptAllowed(claims: TranscriptPrincipal, projectId: string): boolean {
  return claims.scopes.includes('runner:read') && Object.hasOwn(claims.project_roles, projectId);
}

export async function readTranscript(pool: pg.Pool, claims: TranscriptPrincipal, stepId: string, after = 0, claimId?: string) {
  if (!claims.scopes.includes('runner:read')) return [];
  return (await pool.query(
    `SELECT step_run_id, project_id, group_id, runner_id, agent_id, claim_id, generation::double precision AS generation, seq, entry, created_at
     FROM transcripts WHERE step_run_id = $1 AND project_id = ANY($2::text[]) AND seq > $3
       AND ($4::text IS NULL OR claim_id = $4)
     ORDER BY seq LIMIT 1000`,
    [stepId, Object.keys(claims.project_roles), after, claimId ?? null],
  )).rows;
}
