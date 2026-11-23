import { writeCredentialMetadata } from '@runbook/runner-vault/credential-metadata';
import { expect, it, vi } from 'vitest';
import type { Tool } from '@runbook/runner-shared/contracts/catalog';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { loadCeiling, type AgentConfig } from '../src/resources/agents.ts';
import { gatewayResources } from '../src/resources/gateway.ts';
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

it.skipIf(!hasTestDatabase)('derives and confirms the attached Vertex destination, rebinding only on destination changes', async () => {
  const db = await createTestDatabase();
  const claims: UserAssertionClaims = {
    aud: 'runner:install', iss: 'core', jti: 'test', iat: 0, exp: 60, org_id: 'org', user_id: 'human',
    org_role: 'admin', principal: 'human', project_roles: {}, scopes: ['runner:admin'], htm: 'POST', htu: '', bh: '',
  };
  const handle = publicResources(db.pool('api'), {
    role: 'api', listen: { host: '', port: 0 }, urls: { vault: 'https://vault.example' },
    inboundTokens: {}, outboundTokens: { vault: 'vault-process-token' },
  }, async () => {});
  let allowed = false;
  const outbound = vi.spyOn(globalThis, 'fetch').mockImplementation(async (url, init) => {
    expect(url).toBe('https://vault.example/internal/vertex/validate');
    expect(init?.headers).toMatchObject({ authorization: 'Bearer vault-process-token' });
    const { project_id, region } = JSON.parse(String(init?.body));
    return new Response(null, { status: allowed && project_id === 'angler-dev' && ['global', 'us-east5'].includes(region) ? 204 : 403 });
  });
  const save = (body: unknown, id?: string) => handle({
    method: id ? 'PATCH' : 'POST', target: '/runner/v1/providers' + (id ? '/' + id : ''), claims, body,
  });
  const binding = (id: string) => db.admin().query<{ attached_binding_id: string }>(
    'SELECT attached_binding_id FROM providers WHERE id = $1', [id]).then(({ rows }) => rows[0]!.attached_binding_id);
  const base = { name: 'Google identity', type: 'vertex-gemini', auth: 'attached', project_id: 'angler-dev', region: 'global',
    models: [{ id: 'gemini-4-hydrogen', input_usd_per_million: 1.5, output_usd_per_million: 7.5 }] };
  try {
    for (const patch of [
      { project_id: 'angler-dev/path' }, { region: 'us' }, { region: 'us-east5.example' },
      { endpoint: 'https://example.com' }, { endpoint: 'https://us-east5-aiplatform.googleapis.com' }, { credential_id: 'stored' },
      { models: [{ ...base.models[0], output_usd_per_million: -1 }] }, { models: [base.models[0], base.models[0]] },
      { models: [{ ...base.models[0], id: '../model' }] }, { type: 'bedrock' },
    ]) await expect(save({ ...base, ...patch })).rejects.toMatchObject({ status: 422 });
    await expect(save(base)).rejects.toMatchObject({ code: 'catalog.attached_identity_disabled' });
    allowed = true;
    await expect(save({ ...base, region: 'us-east1' })).rejects.toMatchObject({ code: 'catalog.attached_identity_disabled' });
    const saved = await save(base);
    const { id } = saved.body as { id: string };
    expect(saved.body).toMatchObject({ credential_id: null,
      endpoint: 'https://aiplatform.googleapis.com/v1/projects/angler-dev/locations/global/publishers/google/models' });
    const original = await binding(id);
    await save({ name: 'Vertex model' }, id);
    expect(await binding(id)).toBe(original);
    const endpoint = 'https://us-east5-aiplatform.googleapis.com/v1/projects/angler-dev/locations/us-east5/publishers/anthropic/models';
    const change = { type: 'vertex', region: 'us-east5',
      models: [{ id: 'claude-sonnet-5-5', input_usd_per_million: 2, output_usd_per_million: 10 }] };
    await expect(save(change, id)).rejects.toMatchObject({ code: 'catalog.binding_confirmation_required' });
    await expect(save({ ...change, confirm_binding: 'stale' }, id)).rejects.toMatchObject({ code: 'catalog.binding_confirmation_required' });
    expect((await save({ ...change, confirm_binding: endpoint }, id)).body).toMatchObject({ endpoint, type: 'vertex' });
    expect(await binding(id)).not.toBe(original);
    const bindingId = await binding(id);
    await expect(save({ project_id: 'other-project' }, id)).rejects.toMatchObject({ code: 'catalog.binding_confirmation_required' });
    const unlisted = 'https://us-east5-aiplatform.googleapis.com/v1/projects/other-project/locations/us-east5/publishers/anthropic/models';
    await expect(save({ project_id: 'other-project', confirm_binding: unlisted }, id))
      .rejects.toMatchObject({ code: 'catalog.attached_identity_disabled' });
    const proposal = await handle({
      method: 'PATCH', target: '/runner/v1/providers/' + id,
      claims: { ...claims, user_id: 'agent', principal: 'agent' },
      body: { project_id: 'other-project', confirm_binding: unlisted },
    });
    expect(proposal.status).toBe(202);
    await expect(handle({
      method: 'POST', target: '/runner/v1/proposals/' + (proposal.body as { id: string }).id + '/approve', claims, body: undefined,
    })).rejects.toMatchObject({ code: 'catalog.attached_identity_disabled' });
    expect(await binding(id)).toBe(bindingId);
    const events = await db.admin().query("SELECT detail FROM audit_log WHERE resource_id = $1 AND action = 'catalog.updated'", [id]);
    expect(events.rows.map(({ detail }) => detail)).toContainEqual({
      old_binding: { id: original, project_id: 'angler-dev', region: 'global', publisher: 'google' },
      new_binding: { id: bindingId, project_id: 'angler-dev', region: 'us-east5', publisher: 'anthropic' },
    });
    await save({ sharing: { projects: ['p'] } }, id);
    const profile = await handle({ method: 'POST', target: '/runner/v1/profiles', claims,
      body: { name: 'default', provider_id: id, model: 'claude-sonnet-5-5', sharing: { projects: ['p'] } } });
    const profileId = (profile.body as { id: string }).id;
    const config: AgentConfig = { profiles: [profileId], credentials: [], limits: { max_tokens: 100, max_cost_usd: 1, timeout_s: 60 } };
    const pool = db.pool('api');
    const ceiling = await loadCeiling(pool, config, 'p', 'org');
    expect(ceiling.credentials).toEqual([{ id: bindingId, name: 'Vertex model', delivery: ['model'], label: 'read' }]);
    expect(ceiling.profile_models).toEqual({ default: 'claude-sonnet-5-5' });
    await pool.query(`INSERT INTO groups (group_id, name) VALUES ('g', 'default');
      INSERT INTO agents (id, project_id, group_id, name, status) VALUES ('a', 'p', 'g', 'agent', 'active')`);
    await pool.query(`INSERT INTO agent_versions (id, agent_id, version, config, ceiling, created_by)
      VALUES ('v', 'a', 1, $1, $2, 'human')`, [config, ceiling]);
    const verifier = {
      verifyCapability: async () => ({ project_id: 'p', group_id: 'g', agent_id: 'a',
        agent_version: 'v', runner_id: 'r', model_profile: 'default' }),
    } as unknown as Parameters<typeof gatewayResources>[1];
    const routes = gatewayResources(pool, verifier, 'org');
    const model = routes.find(({ path }) => path === '/internal/model/profile')!;
    const resolved = await model.handle({ caller: 'gateway', params: {}, body: { capability: 'fixture', profile: 'default' } });
    expect(resolved.body).toMatchObject({ credentialId: bindingId, inputUsdPerMillion: 2.2, outputUsdPerMillion: 11,
      vertexBinding: { provider: 'vertex', project_id: 'angler-dev', region: 'us-east5', publisher: 'anthropic', endpoint } });
    const worker = routes.find(({ path }) => path === '/internal/worker/step')!;
    expect((await worker.handle({ caller: 'worker', runnerId: 'r', params: {}, body: { capability: 'fixture' } })).body)
      .toMatchObject({ credentials: [{ credential_id: bindingId, delivery: 'model' }] });

  } finally {
    outbound.mockRestore();
    await db.drop();
  }
});
