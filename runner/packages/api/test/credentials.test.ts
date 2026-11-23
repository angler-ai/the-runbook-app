import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { ORG, TOKENS, insertCredential, startHarness } from '../../vault/test/helpers.ts';
import { publicResources } from '../src/resources/public.ts';
import { writeCredentialMetadata } from '@runbook/runner-vault/credential-metadata';
import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { parseSealedBlob } from '@runbook/runner-shared/seal-client';
import { expect, it, vi } from 'vitest';
import { credentialConfiguration, credentialState, credentialResources } from '../src/resources/credentials.ts';

it('credential_destination_change_requires_new_secret', () => {
  const config = (host: string) => credentialConfiguration('api-key',
    { hosts: [host], header_name: 'X-Api-Key', header_format: '{}' }, 'all');
  const old = config('one.example');
  const next = config('two.example');
  expect(credentialState({ binding_hash: old.hash, status: 'active' }, old.hash)).toBe('active');
  expect(credentialState({ binding_hash: old.hash, status: 'active' }, next.hash)).toBe('needs_secret');
});

it('redacts other projects from non-admin credential reads', async () => {
  const row = { id: 'c', sharing: { projects: ['own', 'other'] } };
  const pool = { query: async () => ({ rows: [row] }) } as unknown as Parameters<typeof credentialResources>[0];
  const read = credentialResources(pool, { role: 'api', listen: { host: '', port: 0 }, urls: {}, inboundTokens: {}, outboundTokens: {} });
  const request: Parameters<typeof read>[0] = {
    method: 'GET', target: '/runner/v1/credentials/c', body: undefined,
    claims: {
      iss: 'core', aud: 'runner:install', jti: 'once', iat: 0, exp: 60, org_id: 'org', user_id: 'user',
      org_role: 'member', principal: 'human', project_roles: { own: 'viewer' }, scopes: ['runner:read'],
      htm: 'GET', htu: '/runner/v1/credentials/c', bh: '',
    },
  };
  const segments = ['', 'runner', 'v1', 'credentials'];
  const own = { id: 'c', sharing: { projects: ['own'] } };
  expect((await read(request, [...segments, 'c'])).body).toEqual(own);
  expect((await read(request, segments)).body).toEqual({ items: [own] });
  expect((await read({ ...request, claims: { ...request.claims, org_role: 'admin' } }, [...segments, 'c'])).body).toEqual(row);
});

it.skipIf(!hasTestDatabase)('relays the exact assertion and raw body to the vault and reads its committed metadata', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    const config = { hosts: ['api.example'], header_name: 'x-api-key', header_format: '{}' };
    const hash = credentialConfiguration('api-key', config, 'all').hash;
    await pool.query("INSERT INTO credentials (id,org_id,name,type,config,binding_hash) VALUES ('c','org','key','api-key',$1,$2)",
      [JSON.stringify(config), hash]);
    vi.stubGlobal('fetch', async (_url: string, init: RequestInit) => {
      expect(new Headers(init.headers).get('runbook-assertion')).toBe('signed-assertion');
      const { blob } = JSON.parse(Buffer.from(init.body as Uint8Array).toString());
      const { field, expected_version } = parseSealedBlob(blob);
      await db.pool('vault').query("UPDATE credentials SET secret_meta = $1, status = 'active' WHERE id = 'c'",
        [JSON.stringify({ key: { version: expected_version + 1, fingerprint: 'fixture' } })]);
      return Response.json({ credential_id: 'c', field, version: expected_version + 1, fingerprint: 'fixture' });
    });
    const handle = credentialResources(pool, { role: 'api', listen: { host: '', port: 0 },
      urls: { vault: 'https://vault.example' }, inboundTokens: {}, outboundTokens: {} });
    const status = async () => (await pool.query("SELECT status FROM credentials WHERE id = 'c'")).rows[0]?.status;
    const request: Parameters<typeof handle>[0] = {
      method: 'POST', target: '', body: undefined,
      claims: { iss: 'core', aud: 'runner:install', jti: 'once', iat: 0, exp: 60, org_id: 'org', user_id: 'user',
        org_role: 'admin', principal: 'human', project_roles: {}, scopes: ['runner:admin'], htm: 'POST', htu: '', bh: '' },
    };
    const segments = ['', 'runner', 'v1', 'credentials', 'c'];
    async function seal(field: string, expected_version = 0) {
      const blob = Buffer.from(JSON.stringify({
        v: 1, org_id: 'org', credential_id: 'c', field, expected_version,
        binding_hash: 'fixture', ingest_key_id: 'key', expires_at: 1, enc: 'e', ct: 'c',
      })).toString('base64url');
      const rawBody = Buffer.from(JSON.stringify({ blob }));
      return handle({ ...request, body: { blob }, assertion: { rawBody, token: 'signed-assertion' } },
        [...segments, 'secrets', field]);
    }
    expect(await status()).toBe('needs_secret');
    // Release gate 2026-11-22: sealing a model-provider field name into an
    // api-key credential said only "invalid credential configuration".
    await expect(seal('api_key')).rejects.toMatchObject({
      status: 400, code: 'credential.invalid',
      message: 'api-key credentials have no secret field "api_key"; use "key"',
    });
    expect(await status()).toBe('needs_secret');
    await seal('key');
    expect(await status()).toBe('active');
    const detail = await handle({ ...request, method: 'GET', claims: { ...request.claims, scopes: ['runner:read'] } }, segments);
    expect(detail.body).toMatchObject({ secret_meta: { key: { version: 1 } } });
  } finally {
    vi.unstubAllGlobals();
    await db.drop();
  }
});

it.each(['secrets', 'test'])('preserves Secret Manager destination policy errors on the %s route', async (operation) => {
  const pool = { query: async () => ({ rowCount: 1, rows: [{ type: 'api-key' }] }) } as unknown as Parameters<typeof credentialResources>[0];
  const handle = credentialResources(pool, { role: 'api', listen: { host: '', port: 0 },
    urls: { vault: 'https://vault.example' }, inboundTokens: {}, outboundTokens: {} });
  const claims: UserAssertionClaims = {
    iss: 'core', aud: 'runner:install', jti: 'once', iat: 0, exp: 60, org_id: 'org', user_id: 'user',
    org_role: 'admin', principal: 'human', project_roles: {}, scopes: ['runner:admin'],
    htm: 'POST', htu: '', bh: '',
  };
  const blob = Buffer.from(JSON.stringify({
    v: 1, org_id: 'org', credential_id: 'c', field: 'key', expected_version: 0,
    binding_hash: 'fixture', ingest_key_id: 'key', expires_at: 1, enc: 'e', ct: 'c',
  })).toString('base64url');
  const body = operation === 'secrets' ? { blob } : undefined;
  const rawBody = body ? Buffer.from(JSON.stringify(body)) : Buffer.alloc(0);
  vi.stubGlobal('fetch', async () => Response.json({
    error: { code: 'secret_manager.destination_not_allowed', message: 'private upstream message' },
  }, { status: 403 }));
  try {
    await expect(handle({ method: 'POST', target: '', claims, body, assertion: { rawBody, token: 'signed' } },
      ['', 'runner', 'v1', 'credentials', 'c', operation, 'key'])).rejects.toMatchObject({
      status: 403, code: 'secret_manager.destination_not_allowed',
      message: 'This destination is outside the deployment policy for the Secret Manager reference.',
    });
  } finally {
    vi.unstubAllGlobals();
  }
});

it.skipIf(!hasTestDatabase)('reports revocation when testing a credential and keeps other vault refusals generic', async () => {
  const h = await startHarness({
    core: { baseUrl: 'https://core.example', releaseToken: TOKENS.release },
    gateway: { baseUrl: 'https://gateway.example', token: TOKENS.gateway },
  });
  try {
    const { id } = await insertCredential(h);
    const handle = publicResources(h.db.pool('api'), {
      role: 'api', listen: { host: '', port: 0 }, urls: { vault: h.url },
      inboundTokens: {}, outboundTokens: { vault: TOKENS.api },
    }, async () => {});
    const target = `/runner/v1/credentials/${id}/test`;
    const testCredential = () => handle({
      method: 'POST', target, body: undefined,
      claims: {
        iss: 'https://core.example', aud: 'runner:install_test', iat: 0, exp: 60, jti: 'fixture',
        user_id: 'us_admin', org_id: ORG, principal: 'human', org_role: 'admin', project_roles: {},
        scopes: ['runner:admin'], htm: 'POST', htu: target, bh: '',
      },
      assertion: { token: h.signAssertion(target, ''), rawBody: Buffer.alloc(0) },
    });
    await expect(testCredential()).rejects.toMatchObject({
      status: 409, code: 'credential.vault_unavailable', message: 'credential operation refused',
    });
    await h.db.pool('vault').query("UPDATE credentials SET status = 'revoked', revoked_at = now() WHERE id = $1", [id]);
    await expect(testCredential()).rejects.toMatchObject({
      status: 409, code: 'credential.revoked', message: 'The credential is revoked. Re-create it to use it again.',
    });
  } finally {
    await h.close();
  }
});

it.skipIf(!hasTestDatabase)('relays a scoped agent revocation through the vault', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    await pool.query(`INSERT INTO credentials (id,org_id,name,type,scope,project_id)
      VALUES ('org-key','org','org-key','api-key','org',NULL),
             ('own-key','org','own-key','api-key','project','own'),
             ('other-key','org','other-key','api-key','project','other')`);
    const revoke = vi.fn(async (_url: string, init: RequestInit) => {
      expect(new Headers(init.headers).get('runbook-assertion')).toBe('fresh-assertion');
      await db.pool('vault').query("UPDATE credentials SET status = 'revoked', revoked_at = now() WHERE id = 'own-key'");
      return new Response(null, { status: 204 });
    });
    vi.stubGlobal('fetch', revoke);
    const handle = credentialResources(pool, { role: 'api', listen: { host: '', port: 0 },
      urls: { vault: 'https://vault.example' }, inboundTokens: {}, outboundTokens: {} });
    const claims = {
      iss: 'core', aud: 'runner:install', jti: 'once', iat: 0, exp: 60, org_id: 'org', user_id: 'agent',
      org_role: 'member', principal: 'agent' as const, sponsor_id: 'sponsor', project_roles: { own: 'admin' },
      scopes: ['runner:admin'], htm: 'POST', htu: '', bh: '',
    } satisfies Parameters<typeof handle>[0]['claims'];
    const call = (id: string, options: Partial<Parameters<typeof handle>[0]['claims']> = {}, method = 'POST') => handle(
      { method, target: '', body: undefined, claims: { ...claims, ...options },
        assertion: { token: 'fresh-assertion', rawBody: Buffer.alloc(0) } },
      ['', 'runner', 'v1', 'credentials', id, ...(method === 'POST' ? ['revoke'] : [])],
    );
    await expect(call('own-key', { scopes: ['runner:read'] })).rejects.toMatchObject({ status: 403 });
    await expect(call('other-key')).rejects.toMatchObject({ status: 403 });
    await expect(call('org-key')).rejects.toMatchObject({ status: 403 });
    await expect(call('org-key', { org_role: 'admin' }, 'DELETE')).rejects.toMatchObject({ status: 403 });
    expect(revoke).not.toHaveBeenCalled();
    expect((await call('own-key')).status).toBe(204);
    expect((await call('org-key', { org_role: 'admin' })).status).toBe(204);
    expect(revoke).toHaveBeenCalledTimes(2);
  } finally {
    vi.unstubAllGlobals();
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('defaults a new model credential to read and preserves explicit labels', async () => {
  const db = await createTestDatabase();
  try {
    const handle = credentialResources(db.pool('api'), { role: 'api', listen: { host: '', port: 0 },
      urls: {}, inboundTokens: {}, outboundTokens: {} });
    const claims = {
      iss: 'core', aud: 'runner:install', jti: 'once', iat: 0, exp: 60, org_id: 'org', user_id: 'user',
      org_role: 'admin', principal: 'human', project_roles: {}, scopes: ['runner:admin'], htm: 'POST', htu: '', bh: '',
    } satisfies Parameters<typeof handle>[0]['claims'];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
      expect(url).toMatch(/\/internal\/credential-metadata$/);
      expect(new Headers(init.headers).get('runbook-assertion')).toBe('fresh-assertion');
      return Response.json(await writeCredentialMetadata(db.pool('vault'), claims, JSON.parse(Buffer.from(init.body as Uint8Array).toString())));
    });
    const create = (label?: string) => {
      const body = { type: 'model-provider', name: label ?? 'default model', config: { provider: 'anthropic', endpoint: 'https://api.example' }, label };
      return handle({ method: 'POST', target: '/runner/v1/credentials', claims,
        assertion: { token: 'fresh-assertion', rawBody: Buffer.from(JSON.stringify(body)) }, body,
      }, ['', 'runner', 'v1', 'credentials']);
    };
    expect((await create()).body).toMatchObject({ label: 'read' });
    expect((await create('destructive')).body).toMatchObject({ label: 'destructive' });
  } finally {
    vi.unstubAllGlobals();
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('reserves sharing writes for the vault', async () => {
  const db = await createTestDatabase();
  try {
    await expect(db.pool('api').query("UPDATE credentials SET sharing = DEFAULT WHERE id = 'absent'"))
      .rejects.toMatchObject({ code: '42501' });
    await expect(db.pool('vault').query("UPDATE credentials SET sharing = DEFAULT WHERE id = 'absent'"))
      .resolves.toMatchObject({ rowCount: 0 });
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('applies an approved credential policy through the vault under the approver assertion', async () => {
  const h = await startHarness();
  try {
    const handle = publicResources(h.db.pool('api'), {
      role: 'api', listen: { host: '', port: 0 }, urls: { vault: h.url },
      inboundTokens: {}, outboundTokens: { vault: TOKENS.api },
    }, async () => {});
    const metadata = await h.call('POST', '/internal/credential-metadata', {
      name: 'model', type: 'model-provider', config: { provider: 'anthropic', endpoint: 'https://model.example' },
    });
    expect(metadata.status).toBe(200);
    const id = metadata.json.id as string;
    const claims: UserAssertionClaims = {
      iss: 'https://core.example', aud: 'runner:install_test', iat: 0, exp: 60, jti: 'fixture',
      user_id: 'us_admin', org_id: ORG, principal: 'human', org_role: 'admin', project_roles: {},
      scopes: ['runner:admin'], htm: 'POST', htu: '', bh: '',
    };
    const proposed = await handle({
      method: 'PATCH', target: `/runner/v1/credentials/${id}`,
      claims: { ...claims, user_id: 'agent', principal: 'agent' },
      body: { name: 'reviewed model', sharing: { projects: 'all' } },
    });
    expect(proposed.status).toBe(202);
    const proposalId = (proposed.body as { id: string }).id;
    const target = `/runner/v1/proposals/${proposalId}/approve`;
    const approve = (token: string) => handle({
      method: 'POST', target, claims, body: undefined, assertion: { token, rawBody: Buffer.alloc(0) },
    });
    await expect(approve(h.signAssertion(target + '-other', ''))).rejects.toMatchObject({ status: 401 });
    expect(await approve(h.signAssertion(target, ''))).toEqual({ status: 204 });
    expect((await h.db.pool('api').query('SELECT name, sharing FROM credentials WHERE id = $1', [id])).rows)
      .toEqual([{ name: 'reviewed model', sharing: { projects: 'all' } }]);
    expect((await h.db.pool('api').query('SELECT status, decided_by FROM proposals WHERE id = $1', [proposalId])).rows)
      .toEqual([{ status: 'approved', decided_by: 'us_admin' }]);
  } finally {
    await h.close();
  }
});
