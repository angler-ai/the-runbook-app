import { writeCredentialMetadata } from '@runbook/runner-vault/credential-metadata';
import { expect, it, vi } from 'vitest';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { publicResources } from '../src/resources/public.ts';

it.skipIf(!hasTestDatabase)('authors the catalog, discovers through the gateway path and pins only reviewed snapshots', async () => {
  const db = await createTestDatabase();
  const sync = vi.fn(async () => {});
  const claims: UserAssertionClaims = {
    aud: 'runner:install', iss: 'core', jti: 'test', iat: 0, exp: 60, org_id: 'org', user_id: 'human',
    org_role: 'admin', principal: 'human', project_roles: { p: 'admin' }, scopes: ['runner:admin', 'runner:read'],
    htm: 'POST', htu: '', bh: '',
  };
  const handle = publicResources(db.pool('api'), {
    role: 'api', listen: { host: '', port: 0 }, urls: { vault: 'https://vault.example' },
    inboundTokens: {}, outboundTokens: { vault: 'vault-process-token' },
  }, sync);
  const call = (method: string, path: string, body?: unknown) => handle({
    method, target: '/runner/v1' + path, claims, body,
    ...(body !== undefined && { assertion: { token: 'fixture-assertion', rawBody: Buffer.from(JSON.stringify(body)) } }),
  });
  const tools = [{ name: 'read', inputSchema: { type: 'object' }, description: 'Read' }];
  const outbound = vi.fn(async (url: string, init: RequestInit) => {
    if (url.endsWith('/internal/credential-metadata')) {
      return Response.json(await writeCredentialMetadata(db.pool('vault'), claims, JSON.parse(Buffer.from(init.body as Uint8Array).toString())));
    }
    return Response.json({ tools });
  });
  vi.stubGlobal('fetch', outbound);
  try {
    expect((await call('POST', '/groups', { group_id: 'g', name: 'test', projects: ['p'] })).status).toBe(201);
    expect((await call('PUT', '/policy', { project_limits: { p: { concurrency: 2 } }, run_max_cost_usd: 1 })).status).toBe(200);
    expect((await call('GET', '/policy')).body).toMatchObject({ project_limits: { p: { concurrency: 2 } } });

    const credential = await call('POST', '/credentials', {
      name: 'provider-key', type: 'model-provider', config: { provider: 'anthropic', endpoint: 'https://model.example' },
    });
    const credential_id = (credential.body as { id: string }).id;
    const provider = await call('POST', '/providers', {
      name: 'test-provider', type: 'anthropic', auth: 'stored', endpoint: 'https://model.example',
      credential_id, models: [{ id: 'model', input_usd_per_million: 1, output_usd_per_million: 2 }],
    });
    expect(provider.status).toBe(201);
    const provider_id = (provider.body as { id: string }).id;
    const profile = await call('POST', '/profiles', { name: 'fast', provider_id, model: 'model', sharing: { projects: ['p'] } });
    expect(profile.status).toBe(201);
    const profileId = (profile.body as { id: string }).id;
    expect((await call('PATCH', '/profiles/' + profileId, { name: 'default' })).body).toMatchObject({ name: 'default' });
    expect((await call('GET', '/profiles')).body).toMatchObject({ items: [{ name: 'default' }] });

    const proposed = await handle({
      method: 'POST', target: '/runner/v1/profiles',
      claims: { ...claims, user_id: 'agent', principal: 'agent' },
      body: { name: 'proposed', provider_id, model: 'model' },
    });
    expect(proposed.status).toBe(202);
    const proposalId = (proposed.body as { id: string }).id;
    expect((await call('POST', '/proposals/' + proposalId + '/approve')).status).toBe(204);
    const profiles = (await call('GET', '/profiles')).body as { items: { id: string; name: string }[] };
    const proposedProfile = profiles.items.find(({ name }) => name === 'proposed');
    expect(proposedProfile).toBeDefined();
    await call('DELETE', '/profiles/' + proposedProfile!.id);

    const mcp = await call('POST', '/mcp-servers', { name: 'fixture', url: 'https://mcp.example/', transport: 'streamable-http' });
    const id = (mcp.body as { id: string }).id;
    const discovered = await call('POST', `/mcp-servers/${id}/discover`);
    expect(discovered.body).toMatchObject({ snapshot_id: expect.any(String), tools });
    expect(outbound).toHaveBeenCalledWith('https://vault.example/internal/mcp/discover', expect.objectContaining({
      method: 'POST', body: JSON.stringify({ url: 'https://mcp.example', transport: 'streamable-http' }),
    }));
    const snapshot_id = (discovered.body as { snapshot_id: string }).snapshot_id;
    expect((await call('GET', `/mcp-servers/${id}`)).body).toMatchObject({ pinned_snapshot_id: null });
    expect((await call('POST', `/mcp-servers/${id}/pin`, { snapshot_id })).body).toMatchObject({ pinned_snapshot_id: snapshot_id });
    await call('PATCH', `/mcp-servers/${id}`, { url: 'https://changed.example/' });
    expect((await call('GET', `/mcp-servers/${id}`)).body).toMatchObject({ pinned_snapshot_id: null });
    await expect(call('POST', `/mcp-servers/${id}/pin`, { snapshot_id })).rejects.toMatchObject({ status: 409 });

    expect((await call('GET', '/mcp-servers?presets=true')).body).toMatchObject({
      items: expect.arrayContaining([expect.objectContaining({ name: 'linear-readonly' })]),
    });
    expect((await call('GET', '/credentials?presets=true')).body).toMatchObject({
      items: expect.arrayContaining([expect.objectContaining({ type: 'api-key' })]),
    });
    expect(sync).toHaveBeenCalled();
    expect((await call('DELETE', `/mcp-servers/${id}`)).status).toBe(204);
    expect((await call('GET', `/mcp-servers/${id}`)).status).toBe(404);
    expect((await call('DELETE', '/profiles/' + profileId)).status).toBe(204);
    expect((await call('DELETE', '/providers/' + provider_id)).status).toBe(204);
    expect((await call('DELETE', '/groups/g')).status).toBe(204);
  } finally {
    vi.unstubAllGlobals();
    await db.drop();
  }
});
