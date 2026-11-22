import { createHash } from 'node:crypto';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { JWT_TYP_USER, type UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { generateSigningKey, signJwt } from '@runbook/runner-shared/test-utils/jwt';
import { createTestDatabase, hasTestDatabase, type TestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { AssertionVerifier } from '../src/auth.ts';
import { authorize } from '../src/authorize.ts';

const now = 2_000_000_000;
const body = Buffer.from('{"foo":"bar"}');
const claims: UserAssertionClaims = {
  iss: 'https://core.example', aud: 'runner:install', jti: 'once', iat: now, exp: now + 60,
  org_id: 'org', user_id: 'user', principal: 'human', org_role: 'member', project_roles: { p: 'editor', q: 'viewer' },
  scopes: ['runner:admin', 'runner:read'], htm: 'POST', htu: '/runner/v1/projects/p/agents',
  bh: createHash('sha256').update(body).digest('base64url'),
};
describe.skipIf(!hasTestDatabase)('assertion verification across API replicas', () => {
  let db: TestDatabase;
  beforeAll(async () => { db = await createTestDatabase(); });
  afterAll(async () => { await db?.drop(); });

  function setup() {
    const key = generateSigningKey('user-test');
    const options = {
      coreUrl: claims.iss, installId: 'install', orgId: 'org', now: () => now * 1000,
      fetch: async () => Response.json({ keys: [key.jwk] }), db: db.pool('api'),
    };
    return {
      first: new AssertionVerifier(options), second: new AssertionVerifier(options),
      input: { token: signJwt(key, JWT_TYP_USER, claims), method: claims.htm, target: claims.htu, body },
    };
  }

  it('replayed_assertion_refused_by_a_second_api_replica', async () => {
    const { first, second, input } = setup();
    await expect(first.verifyAssertion(input)).resolves.toEqual(claims);
    await expect(second.verifyAssertion(input)).rejects.toMatchObject({ status: 401, code: 'assertion.replayed' });
  });
  it('body_hash_mismatch_refused', async () => {
    const { first, input } = setup();
    await expect(first.verifyAssertion({ ...input, body: Buffer.from('{}') })).rejects.toMatchObject({ status: 401 });
  });
});
it('editor_can_edit_instructions_not_credentials', () => {
  expect(authorize(claims, 'edit', { kind: 'agent', projectId: 'p' })).toBe('allow');
  expect(authorize(claims, 'widen', { kind: 'agent', projectId: 'p' })).toBe('deny');
  expect(authorize(claims, 'edit', { kind: 'org' })).toBe('deny');
});
it('viewer_sees_only_own_projects_agents', () => {
  expect(authorize(claims, 'read', { kind: 'agent', projectId: 'q' })).toBe('allow');
  expect(authorize(claims, 'read', { kind: 'agent', projectId: 'unrelated' })).toBe('deny');
});
it('agent_principal_widening_becomes_proposal', () => {
  expect(authorize({ ...claims, principal: 'agent' }, 'widen', { kind: 'agent', projectId: 'p' })).toBe('proposal');
});
it('agent_principal_cannot_set_secret', () => {
  expect(authorize({ ...claims, principal: 'agent', org_role: 'owner' }, 'secret', { kind: 'org' })).toBe('deny');
});
