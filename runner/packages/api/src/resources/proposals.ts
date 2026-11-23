import { randomUUID } from 'node:crypto';
import type pg from 'pg';
import { withTransaction } from '@runbook/runner-shared/db';
import { HttpError } from '@runbook/runner-shared/http';
import { authorize, type Resource } from '../authorize.ts';
import type { PublicRequest } from '../server.ts';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';

export function proposalExpiry(now: Date): Date {
  return new Date(now.getTime() + 7 * 24 * 60 * 60 * 1000);
}

export async function createProposal(pool: pg.Pool, claims: UserAssertionClaims, input: {
  kind: string; targetType: string; targetId: string | null; diff: Record<string, unknown>;
}, now = new Date()) {
  const id = randomUUID();
  const expiresAt = proposalExpiry(now);
  await pool.query(
    `INSERT INTO proposals(id, kind, target_type, target_id, diff, proposed_by, sponsor_id, expires_at)
     VALUES ($1,$2,$3,$4,$5,$6,$7,$8)`,
    [id, input.kind, input.targetType, input.targetId, JSON.stringify(input.diff), claims.user_id, claims.sponsor_id, expiresAt],
  );
  return { id, expires_at: expiresAt.toISOString() };
}


interface Proposal {
  id: string;
  kind: string;
  target_id: string | null;
  diff: Record<string, unknown>;
  status: string;
  expires_at: Date;
}

type Apply = (request: PublicRequest, proposal: Proposal, client: pg.PoolClient) => Promise<unknown>;

export function proposalResources(pool: pg.Pool, apply: Apply, sync: () => Promise<void>) {
  return async (request: PublicRequest, id?: string, operation?: string) => {
    const { claims, method } = request;
    const orgAdmin = claims.org_role === 'owner' || claims.org_role === 'admin';
    if (method === 'GET' && !operation) {
      const { rows } = await pool.query(
        `SELECT *, CASE WHEN status = 'open' AND expires_at <= now() THEN 'expired' ELSE status END AS status
         FROM proposals WHERE ($1::text IS NULL OR id = $1) AND (
           (kind = 'agent.change' AND diff->>'project_id' = ANY($2::text[])) OR
           (kind IN ('credential.change', 'catalog.change') AND $3)
         ) ORDER BY created_at DESC LIMIT 1000`,
        [id ?? null, Object.keys(claims.project_roles), orgAdmin],
      );
      return id ? { status: rows.length ? 200 : 404, body: rows[0] } : { body: { items: rows } };
    }
    if (!id || method !== 'POST' || (operation !== 'approve' && operation !== 'reject')) {
      throw new HttpError(404, 'proposal.not_found', 'proposal unavailable');
    }
    const status = await withTransaction(pool, async (client) => {
      const { rows: [proposal] } = await client.query<Proposal>('SELECT * FROM proposals WHERE id = $1 FOR UPDATE', [id]);
      if (!proposal) throw new HttpError(404, 'proposal.not_found', 'proposal unavailable');
      let resource: Resource;
      if (proposal.kind === 'agent.change' && typeof proposal.diff.project_id === 'string') {
        resource = { kind: 'agent', projectId: proposal.diff.project_id };
      } else if (proposal.kind === 'credential.change' || proposal.kind === 'catalog.change') {
        resource = { kind: 'org' };
      } else {
        throw new HttpError(422, 'proposal.invalid', 'invalid proposal');
      }
      if (authorize(claims, 'approve', resource) !== 'allow') throw new HttpError(403, 'auth.denied', 'runner resource unavailable');
      if (proposal.status === 'expired') return 410;
      if (proposal.status !== 'open') throw new HttpError(409, 'proposal.closed', 'proposal already decided');
      if (proposal.expires_at.getTime() <= Date.now()) {
        await client.query("UPDATE proposals SET status = 'expired' WHERE id = $1", [id]);
        return 410;
      }
      if (operation === 'approve') await apply(request, proposal, client);
      const decision = operation === 'approve' ? 'approved' : 'rejected';
      await client.query('UPDATE proposals SET status = $2, decided_at = now(), decided_by = $3 WHERE id = $1',
        [id, decision, claims.user_id]);
      await client.query(
        `INSERT INTO audit_log(id, actor, action, resource_type, resource_id) VALUES ($1,$2,$3,'proposal',$4)`,
        [randomUUID(), JSON.stringify({ kind: 'user', id: claims.user_id }), `proposal.${decision}`, id],
      );
      return 204;
    });
    if (status === 204 && operation === 'approve') await sync();
    return { status };
  };
}
