import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { publicResources } from '../src/resources/public.ts';
import { expect, it, vi } from 'vitest';
import { proposalExpiry } from '../src/resources/proposals.ts';

it('proposal_expires_after_7_days', () => {
  expect(proposalExpiry(new Date('2026-11-20T00:00:00Z')).toISOString()).toBe('2026-11-27T00:00:00.000Z');
});

it.skipIf(!hasTestDatabase)('applies proposals only after authorized human review and revalidates the change', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    await pool.query(`INSERT INTO groups(group_id,name,projects) VALUES ('g','group','["p"]')`);
    const sync = vi.fn(async () => {});
    const handle = publicResources(pool, { role: 'api', listen: { host: '', port: 0 }, urls: {}, inboundTokens: {}, outboundTokens: {} }, sync);
    const agent: UserAssertionClaims = {
      iss: 'core', aud: 'runner:install', jti: 'test', iat: 0, exp: 60, org_id: 'org', user_id: 'agent',
      org_role: 'member', principal: 'agent', project_roles: { p: 'editor' }, scopes: ['runner:read', 'runner:admin'],
      htm: 'POST', htu: '', bh: '',
    };
    const human = { ...agent, session_hash: 'browser', principal: 'human' as const, user_id: 'reviewer', project_roles: { p: 'admin' as const } };
    const create = (body: unknown) => handle({ method: 'POST', target: '/runner/v1/projects/p/agents', claims: agent, body });
    async function proposal(body: unknown) {
      const result = await create(body);
      expect(result.status).toBe(202);
      return (result.body as { id: string }).id;
    }
    const review = (id: string, claims: UserAssertionClaims = human, action = 'approve') => handle({
      method: 'POST', target: `/runner/v1/proposals/${id}/${action}`, claims, body: undefined,
    });
    const id = await proposal({ name: 'candidate', group_id: 'g' });
    await expect(review(id, { ...human, project_roles: { p: 'editor' } })).rejects.toMatchObject({ status: 403 });
    await expect(review(id, { ...human, session_hash: undefined })).rejects.toMatchObject({ status: 403 });
    await expect(review(id, agent)).rejects.toMatchObject({ status: 403 });
    expect((await review(id)).status).toBe(204);
    expect((await pool.query("SELECT name, status FROM agents WHERE name = 'candidate'")).rows).toEqual([{ name: 'candidate', status: 'draft' }]);
    expect((await pool.query('SELECT status, decided_by FROM proposals WHERE id = $1', [id])).rows)
      .toEqual([{ status: 'approved', decided_by: 'reviewer' }]);
    await expect(review(id)).rejects.toMatchObject({ status: 409 });

    const invalid = await proposal({ name: 'second', group_id: 'g', profiles: ['missing'] });
    await expect(review(invalid)).rejects.toMatchObject({ status: 422 });
    expect((await pool.query('SELECT status FROM proposals WHERE id = $1', [invalid])).rows).toEqual([{ status: 'open' }]);
    const expired = await proposal({ name: 'expired', group_id: 'g' });
    await pool.query('UPDATE proposals SET expires_at = now() - interval \'1 second\' WHERE id = $1', [expired]);
    expect((await review(expired)).status).toBe(410);
    expect((await review(invalid, human, 'reject')).status).toBe(204);
    expect((await pool.query('SELECT status, decided_by FROM proposals WHERE id = $1', [invalid])).rows)
      .toEqual([{ status: 'rejected', decided_by: 'reviewer' }]);
  } finally {
    await db.drop();
  }
});
