import { writeCredentialMetadata } from '@runbook/runner-vault/credential-metadata';
import { expect, it, vi } from 'vitest';
import type { Tool } from '@runbook/runner-shared/contracts/catalog';
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
  let tools: Tool[] = [
    { name: 'read', inputSchema: { type: 'object', properties: {} }, description: 'Read' },
    { name: 'retired', inputSchema: { type: 'object' }, description: 'Retired' },
    { name: 'stable', inputSchema: { type: 'object', properties: {} }, description: 'Stable' },
  ];
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

    const mcp = await call('POST', '/mcp-servers', { name: 'fixture', url: 'https://mcp.example/', transport: 'streamable-http',
      sharing: { projects: ['p'] }, tool_labels: { read: 'read' }, never_allowed: ['retired'] });
    const id = (mcp.body as { id: string }).id;
    const discovered = await call('POST', `/mcp-servers/${id}/discover`);
    expect(discovered.body).toMatchObject({ snapshot_id: expect.any(String), tools });
    expect(outbound).toHaveBeenCalledWith('https://vault.example/internal/mcp/discover', expect.objectContaining({
      method: 'POST', body: JSON.stringify({ url: 'https://mcp.example', transport: 'streamable-http' }),
    }));
    const snapshot_id = (discovered.body as { snapshot_id: string }).snapshot_id;
    expect((await call('GET', `/mcp-servers/${id}`)).body).toMatchObject({ pinned_snapshot_id: null });
    expect((await call('POST', `/mcp-servers/${id}/pin`, { snapshot_id })).body).toMatchObject({ pinned_snapshot_id: snapshot_id });
    const serverPath = `/mcp-servers/${id}`;
    const pickerPath = '/projects/p/mcp-tools';
    const read = (path: string, project_roles: UserAssertionClaims['project_roles']) => handle({
      method: 'GET', target: '/runner/v1' + path, body: undefined,
      claims: { ...claims, org_role: 'member', project_roles, scopes: ['runner:read'] },
    });

    expect((await read(serverPath + '/tools', { p: 'viewer' })).body).toMatchObject({
      snapshot: { snapshot_id, tools },
    });
    expect((await read(pickerPath, { p: 'viewer' })).body).toEqual({
      items: [{ id, name: 'fixture', pinned_snapshot_id: snapshot_id,
        tools: [{ name: 'read', label: 'read' }, { name: 'stable', label: 'destructive' }] }],
    });
    await expect(read(pickerPath, {})).rejects.toMatchObject({ status: 403 });
    await expect(read(serverPath + '/tools', { other: 'viewer' })).rejects.toMatchObject({ status: 404 });
    await expect(read(serverPath + '/drift', { other: 'viewer' })).rejects.toMatchObject({ status: 404 });
    expect((await read('/projects/other/mcp-tools', { other: 'viewer' })).body).toEqual({ items: [] });
    expect((await call('GET', serverPath)).body).toMatchObject({ pinned_tool_count: 3, drift: false });

    const reordered = [...tools].reverse();
    tools = reordered;
    await call('POST', serverPath + '/discover');
    expect((await call('GET', serverPath)).body).toMatchObject({ drift: false });
    expect((await call('GET', serverPath + '/drift')).body).toMatchObject({
      changes: { added: [], changed: [], removed: [] },
    });

    const [stable, retired, previous] = reordered;
    tools = [
      { ...previous!, inputSchema: { type: 'object', required: ['query'] } },
      { name: 'new', inputSchema: { type: 'object' }, description: 'New' },
      { ...stable!, inputSchema: { properties: {}, type: 'object' } },
    ];
    const latest = await call('POST', serverPath + '/discover');
    expect((await read(serverPath + '/drift', { p: 'viewer' })).body).toEqual({
      latest: latest.body,
      changes: {
        added: ['new'], changed: [{ name: 'read', before: previous, schema_changed: true }],
        removed: [retired],
      },
    });
    expect((await call('GET', '/mcp-servers')).body).toMatchObject({
      items: [{ id, pinned_tool_count: 3, drift: true }],
    });
    expect((await read(pickerPath, { p: 'viewer' })).body).toMatchObject({
      items: [{ tools: [{ name: 'read' }, { name: 'stable' }] }],
    });

    const reviewed = (latest.body as { snapshot_id: string }).snapshot_id;
    await expect(call('POST', serverPath + '/pin', {
      snapshot_id: 'bad-pin', tool_labels: { read: 'write' }, never_allowed: ['read'],
    })).rejects.toMatchObject({ status: 409 });
    expect((await call('GET', serverPath)).body).toMatchObject({
      pinned_snapshot_id: snapshot_id, tool_labels: { read: 'read' }, never_allowed: ['retired'],
    });
    expect((await call('POST', serverPath + '/pin', {
      snapshot_id: reviewed, tool_labels: { read: 'read', new: 'write' }, never_allowed: ['new'],
    })).body).toMatchObject({
      pinned_snapshot_id: reviewed, tool_labels: { read: 'read', new: 'write' }, never_allowed: ['new'],
    });

    await call('PATCH', `/mcp-servers/${id}`, { url: 'https://changed.example/' });
    expect((await call('GET', serverPath + '/tools')).body).toEqual({ snapshot: null });
    expect((await call('GET', serverPath + '/drift')).body).toEqual({
      latest: null, changes: { added: [], changed: [], removed: [] },
    });
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
    expect((await read(pickerPath, { p: 'viewer' })).body).toEqual({ items: [] });
    expect((await call('DELETE', '/profiles/' + profileId)).status).toBe(204);
    expect((await call('DELETE', '/providers/' + provider_id)).status).toBe(204);
    expect((await call('DELETE', '/groups/g')).status).toBe(204);
  } finally {
    vi.unstubAllGlobals();
    await db.drop();
  }
});
