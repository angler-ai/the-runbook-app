import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { publicResources } from '../src/resources/public.ts';
import { gatewayResources } from '../src/resources/gateway.ts';
import { expect, it } from 'vitest';
import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { computeCeiling, credentialAllowed, loadCeiling, saveAgentVersion } from '../src/resources/agents.ts';

const config = {
  profiles: ['profile'],
  credentials: [{ credential_id: 'old', delivery: 'proxy' as const }],
  limits: { max_cost_usd: 0.5, max_tokens: 100, timeout_s: 60 },
};
const catalog = {
  profiles: [{ id: 'profile', name: 'fast' }],
  credentials: [{ id: 'old', name: 'datadog', sharing: { projects: ['p'] }, label: 'write' as const, delivery: ['proxy' as const] }],
};

it('ceiling_includes_labels_and_allowed_profiles', () => {
  expect(computeCeiling(config, catalog, 'p')).toMatchObject({
    profiles: ['fast'], credentials: [{ id: 'old', label: 'write', delivery: ['proxy'] }],
  });
});
it('agent_cannot_reference_unshared_credential', () => {
  expect(credentialAllowed(catalog.credentials[0]!, 'q', 'proxy')).toBe(false);
});
it('references_are_by_id', () => {
  expect(() => computeCeiling(config, { ...catalog, credentials: [{ ...catalog.credentials[0]!, id: 'new' }] }, 'p'))
    .toThrow('credential unavailable');
});

it.skipIf(!hasTestDatabase)('saving_agent_creates_immutable_version', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    await pool.query("INSERT INTO agents(id, project_id, group_id, name) VALUES ('a','p','g','agent')");
    const input = { agentId: 'a', projectId: 'p', actorId: 'user', config, ceiling: computeCeiling(config, catalog, 'p') };
    const saved = await Promise.all([saveAgentVersion(pool, input), saveAgentVersion(pool, input)]);
    expect(saved.map(({ version }) => version).sort()).toEqual([1, 2]);
    await expect(pool.query("UPDATE agent_versions SET config = '{}'")).rejects.toMatchObject({ code: '42501' });
  } finally {
    await db.drop();
  }
});

it.each([
  { host: 'https://api.example', label: 'read' },
  { host: '*.example', label: 'read' },
  { host: 'a..example', label: 'read' },
  { host: 'api.example:443', label: 'read' },
  { host: 'api.example', label: 'destructive' },
] as const)('rejects invalid agent egress entry %j', (entry) => {
  expect(() => computeCeiling({ ...config, egress: [entry] }, catalog, 'p')).toThrow('invalid agent egress');
});

it.skipIf(!hasTestDatabase)('only the gateway can write a capability-bound tool call record', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    const verifier = { verifyCapability: async () => ({
      sub: 'step', runner_id: 'runner', group_id: 'group', project_id: 'project', agent_id: 'agent',
    }) } as unknown as Parameters<typeof gatewayResources>[1];
    const route = gatewayResources(pool, verifier).find(({ path }) => path === '/internal/mcp/tool-calls');
    expect(route?.callers).toEqual(['gateway']);
    const body = { capability: 'fixture', server_id: 'server', tool: 'list', args_redacted: { key: '[REDACTED]' },
      args_bytes: 14, result_bytes: 20, duration_ms: 5 };
    expect(await route?.handle({ caller: 'gateway', body, params: {} })).toEqual({ status: 204 });
    expect((await pool.query('SELECT step_run_id, runner_id, tool, args_redacted, args_bytes FROM tool_calls')).rows)
      .toEqual([{ step_run_id: 'step', runner_id: 'runner', tool: 'list', args_redacted: body.args_redacted, args_bytes: 14 }]);
    await expect(db.pool('worker').query(`INSERT INTO tool_calls(step_run_id,group_id,runner_id,project_id,agent_id,tool)
      VALUES ('step','group','runner','project','agent','forged')`)).rejects.toMatchObject({ code: '42501' });
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('gateway policy uses pinned configuration and live labels without trusting worker flags', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    await db.admin().query(`INSERT INTO credentials(id,org_id,name,type,label,sharing,delivery,status)
      VALUES ('c','org','key','api-key','destructive','{"projects":["p"]}','["model","proxy","mcp"]','active')`);
    await pool.query("INSERT INTO agents(id,project_id,group_id,name,status) VALUES ('a','p','g','agent','active')");
    const ceiling = { profiles: [], credentials: [], tools: [], hosts: [], max_cost_usd: 1, max_tokens: 100, timeout_s: 60 };
    const version = await saveAgentVersion(pool, {
      agentId: 'a', projectId: 'p', actorId: 'user', ceiling,
      config: { ...config, profiles: [], egress: [{ host: 'api.example', label: 'write' }],
        credentials: [{ credential_id: 'c', delivery: 'proxy' }], mcp: [{ server_id: 'mcp', tool: 'erase' }] },
    });
    await pool.query(`INSERT INTO mcp_servers(id,name,url,transport,sharing,tool_labels)
      VALUES ('mcp','example','https://mcp.example','streamable-http','{"projects":["p"]}','{"erase":"destructive"}')`);
    await pool.query(`INSERT INTO mcp_tool_snapshots(id,server_id,hash,tools,pinned_by)
      VALUES ('pin','mcp','\\x00','[{"name":"erase","inputSchema":{"type":"object"}}]','user')`);
    await pool.query("UPDATE mcp_servers SET pinned_snapshot_id = 'pin' WHERE id = 'mcp'");
    await pool.query(`UPDATE policy SET default_egress = '[{"host":"api.example","label":"write"}]'`);
    const claims = { destructive: false, network: ['blocked.example'] as string[] | null, connections: ['c'], sub: 'step', runner_id: 'runner', group_id: 'g', project_id: 'p', agent_id: 'a', agent_version: version.id };
    const verifier = { verifyCapability: async () => claims } as unknown as Parameters<typeof gatewayResources>[1];
    const routes = gatewayResources(pool, verifier, 'org');
    const body = { capability: 'fixture', destructive: true, hosts: ['api.example'] };
    const invoke = (path: string) => routes.find((route) => route.path === path)?.handle({ caller: 'gateway', params: {}, body });
    expect((await invoke('/internal/egress/policy'))?.body).toMatchObject({
      destructive: false, credentialLabels: { c: 'destructive' }, layers: [{ 'api.example': 'write' }, { 'api.example': 'write' }, {}],
    });
    expect((await invoke('/internal/mcp/snapshot'))?.body).toMatchObject({ destructive: false, servers: [] });
    claims.destructive = true;
    claims.network = null;
    expect((await invoke('/internal/egress/policy'))?.body).toMatchObject({
      destructive: true, credentialLabels: { c: 'destructive' },
      layers: [{ 'api.example': 'write' }, { 'api.example': 'write' }, { 'api.example': 'write' }],
    });
    expect((await invoke('/internal/mcp/snapshot'))?.body).toMatchObject({
      destructive: true, servers: [{ id: 'mcp', tools: [{ label: 'destructive' }] }],
    });
    claims.connections = [];
    expect((await invoke('/internal/egress/policy'))?.body).toEqual(expect.objectContaining({ credentialLabels: {} }));
    claims.destructive = false;
    await pool.query(`UPDATE mcp_servers SET tool_labels = '{"erase":"read"}'`);
    expect((await invoke('/internal/mcp/snapshot'))?.body).toMatchObject({
      destructive: false, servers: [{ id: 'mcp', tools: [{ label: 'read', tool: { name: 'erase' } }] }],
    });
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('a step always receives its profile credential and only its permitted other connections', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    await db.admin().query(`INSERT INTO groups(group_id,name) VALUES ('g','default');
      INSERT INTO credentials(id,org_id,name,type,label,sharing,delivery,status)
      VALUES ('model','org','model key','api-key','read','{"projects":["p"]}','["model"]','active'),
             ('x','org','first','api-key','read','{"projects":["p"]}','["proxy"]','active'),
             ('y','org','second','api-key','read','{"projects":["p"]}','["proxy"]','active');
      INSERT INTO providers(id,name,type,auth,credential_id,endpoint,models,sharing)
      VALUES ('provider','provider','openai-compatible','stored','model','https://provider.example',
              '[{"id":"m","input_usd_per_million":1,"output_usd_per_million":1}]','{"projects":["p"]}');
      INSERT INTO profiles(id,name,provider_id,model,sharing)
      VALUES ('p1','fast','provider','m','{"projects":["p"]}'), ('p2','deep','provider','m','{"projects":["p"]}');
      INSERT INTO agents(id,project_id,group_id,name,status) VALUES ('a','p','g','agent','active')`);
    const agent = { ...config, profiles: ['p1', 'p2'],
      credentials: ['x', 'y'].map((credential_id) => ({ credential_id, delivery: 'proxy' as const })) };
    const ceiling = await loadCeiling(pool, agent, 'p', 'org');
    expect(ceiling.credentials).toContainEqual({ id: 'model', name: 'model key', label: 'read', delivery: ['model'] });
    const version = await saveAgentVersion(pool, { agentId: 'a', projectId: 'p', actorId: 'user', ceiling, config: agent });
    expect(ceiling.profile_models).toEqual({ fast: 'm', deep: 'm' });
    const claims = { reach: undefined as 'read' | undefined, model_id: undefined as string | undefined, destructive: false, network: null, connections: null as string[] | null,
      model_profile: 'deep', max_tokens: 20, timeout_ms: 5000, sub: 'step', runner_id: 'r', group_id: 'g',
      project_id: 'p', agent_id: 'a', agent_version: version.id };
    const verifier = { verifyCapability: async () => claims } as unknown as Parameters<typeof gatewayResources>[1];
    const routes = gatewayResources(pool, verifier, 'org');
    const worker = routes.find(({ path }) => path === '/internal/worker/step')!;
    const model = routes.find(({ path }) => path === '/internal/model/profile')!;
    const step = () => worker.handle({ caller: 'worker', runnerId: 'r', params: {}, body: { capability: 'fixture' } });
    for (const [narrowing, expected] of [[null, ['model', 'x', 'y']], [['x'], ['model', 'x']], [[], ['model']]] as const) {
      claims.connections = narrowing === null ? null : [...narrowing];
      const response = await step();
      expect(response.body).toMatchObject({ profile: 'deep', max_output_tokens: 20, limits: { timeout_s: 5 } });
      expect((response.body as { credentials: { credential_id: string }[] }).credentials
        .map(({ credential_id }) => credential_id).sort()).toEqual(expected);
    }
    await expect(model.handle({ caller: 'gateway', params: {}, body: { capability: 'fixture', profile: 'fast' } }))
      .rejects.toMatchObject({ status: 403 });
    const selected = await model.handle({ caller: 'gateway', params: {}, body: { capability: 'fixture', profile: 'deep' } });
    const mark = routes.find(({ path }) => path === '/internal/model/verified')!;
    const verify = () => mark.handle({ caller: 'gateway', params: {}, body: {
      capability: 'fixture', profile: 'deep', verification: (selected.body as { verification: string }).verification,
    } });
    expect((await pool.query("SELECT verified_at FROM profiles WHERE id = 'p2'")).rows).toEqual([{ verified_at: null }]);
    await verify();
    expect((await pool.query("SELECT verified_at FROM profiles WHERE id = 'p2'")).rows)
      .toEqual([{ verified_at: expect.any(Date) }]);
    await pool.query("UPDATE profiles SET options = '{}', updated_at = now() WHERE id = 'p2'");
    await expect(verify()).rejects.toMatchObject({ status: 409 });

    claims.reach = 'read';
    claims.connections = ['model'];
    claims.model_id = 'm';
    expect((await step()).body).toMatchObject({ model: 'm' });
    await pool.query(`UPDATE providers SET models = '[{"id":"changed","input_usd_per_million":1,"output_usd_per_million":1}]'
      WHERE id = 'provider'`);
    await pool.query("UPDATE profiles SET model = 'changed' WHERE id = 'p2'");
    await expect(step()).rejects.toMatchObject({ status: 403 });
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('project audit is limited to its admins and versions list distinct runs', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    await pool.query(`INSERT INTO agents(id,project_id,group_id,name,status)
        VALUES ('a','p','g','agent','active'), ('b','q','g','other','active');
      INSERT INTO audit_log(id,actor,action,resource_type,resource_id)
        VALUES ('audit-a','{"id":"human"}','agent.updated','agent','a'),
               ('audit-b','{"id":"human"}','agent.updated','agent','b'),
               ('org-audit','{"id":"human"}','credential.updated','credential','a')`);
    const ceiling = computeCeiling(config, catalog, 'p');
    const version = await saveAgentVersion(pool, { agentId: 'a', projectId: 'p', actorId: 'user', ceiling, config });
    await db.admin().query(`INSERT INTO slots(id,step_run_id,group_id,runner_id,project_id,agent_id,execution_id,agent_version_id)
      VALUES ('one','s1','g','r','p','a','run-one',$1), ('two','s2','g','r','p','a','run-one',$1),
             ('foreign','s3','g','r','q','a','private',$1)`, [version.id]);
    const handle = publicResources(pool, {
      role: 'api', listen: { host: '', port: 0 }, urls: {}, inboundTokens: {}, outboundTokens: {},
    }, async () => undefined);
    const claims: UserAssertionClaims = {
      aud: 'runner:install', iss: 'core', jti: 'test', iat: 0, exp: 60, org_id: 'org', user_id: 'human',
      org_role: 'member', principal: 'human', project_roles: { p: 'admin', q: 'viewer' },
      scopes: ['runner:read', 'runner:admin'], htm: 'GET', htu: '', bh: '',
    };
    const get = (target: string) => handle({ method: 'GET', target: '/runner/v1' + target, claims, body: undefined });
    const own = await get('/projects/p/agents/a/audit');
    expect((own.body as { items: { id: string }[] }).items.map(({ id }) => id)).toEqual(['audit-a', expect.any(String)]);
    await expect(get('/projects/q/agents/b/audit')).rejects.toMatchObject({ status: 403 });
    await expect(get('/audit')).rejects.toMatchObject({ status: 403 });
    expect((await get('/projects/p/agents/a/versions')).body).toMatchObject({
      items: [{ id: version.id, run_count: 1, runs: [{ execution_id: 'run-one' }] }],
    });
    claims.project_roles.p = 'editor';
    await expect(get('/projects/p/agents/a/audit')).rejects.toMatchObject({ status: 403 });
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('agent saves pin the project skill body for the capability version', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    await db.admin().query(`INSERT INTO groups(group_id,name,projects) VALUES ('g','default','["p"]');
      INSERT INTO credentials(id,org_id,name,type,label,sharing,delivery,status)
      VALUES ('model','org','model key','api-key','read','{"projects":["p"]}','["model"]','active');
      INSERT INTO providers(id,name,type,auth,credential_id,endpoint,models,sharing)
      VALUES ('provider','provider','openai-compatible','stored','model','https://provider.example',
              '[{"id":"m","input_usd_per_million":1,"output_usd_per_million":1}]','{"projects":["p"]}');
      INSERT INTO profiles(id,name,provider_id,model,sharing)
      VALUES ('p1','fast','provider','m','{"projects":["p"]}')`);
    let skill = { name: 'checks', hash: 'a'.repeat(64), body: 'Original skill' };
    const reads: string[] = [];
    const directory = {
      coreUrl: 'https://core.fixture', installId: 'install', token: 'directory-token',
      fetch: async (input: Parameters<typeof fetch>[0], init?: RequestInit) => {
        const request = new Request(input, init);
        expect(request.headers.get('authorization')).toBe('Bearer directory-token');
        reads.push(new URL(request.url).pathname);
        return Response.json(skill);
      },
    };
    const handle = publicResources(pool, {
      role: 'api', listen: { host: '', port: 0 }, urls: {}, inboundTokens: {}, outboundTokens: {},
    }, async () => undefined, directory);
    const claims: UserAssertionClaims = {
      aud: 'runner:install', iss: 'core', jti: 'test', iat: 0, exp: 60, org_id: 'org', user_id: 'human',
      org_role: 'owner', principal: 'human', session_hash: 'browser', project_roles: { p: 'admin' },
      scopes: ['runner:read', 'runner:admin'], htm: 'POST', htu: '', bh: '',
    };
    const root = '/runner/v1/projects/p/agents';
    const created = await handle({ method: 'POST', target: root, claims, body: {
      name: 'agent', group_id: 'g', status: 'active', profiles: ['p1'], skills: ['checks'],
    } });
    const first = created.body as { id: string; version_id: string };
    skill = { ...skill, hash: 'b'.repeat(64), body: 'Updated skill' };
    const capability = { sub: 'step', runner_id: 'r', group_id: 'g', project_id: 'p',
      agent_id: first.id, agent_version: first.version_id, connections: null, destructive: false };
    const verifier = { verifyCapability: async () => capability } as unknown as Parameters<typeof gatewayResources>[1];
    const worker = gatewayResources(pool, verifier, 'org').find(({ path }) => path === '/internal/worker/step')!;
    const load = () => worker.handle({ caller: 'worker', runnerId: 'r', params: {}, body: { capability: 'fixture' } });
    const original = { name: 'checks', hash: 'a'.repeat(64), body: 'Original skill' };
    expect((await load()).body).toMatchObject({ skill_pins: [original] });
    expect(reads).toEqual(['/v1/runner-installs/install/projects/p/skills/checks']);
    const proposed = await handle({ method: 'PATCH', target: `${root}/${first.id}`,
      claims: { ...claims, principal: 'agent', sponsor_id: 'human' },
      body: { instructions: 'Agent proposed instructions', skills: ['checks'] } });
    expect(proposed.status).toBe(202);
    expect((await pool.query('SELECT current_version_id FROM agents WHERE id = $1', [first.id])).rows)
      .toEqual([{ current_version_id: first.version_id }]);
    expect(reads).toHaveLength(1);
    const edit = await handle({ method: 'PATCH', target: `${root}/${first.id}`, claims, body: { instructions: 'Use metrics' } });
    expect((await load()).body).toMatchObject({ skill_pins: [original] });
    capability.agent_version = (edit.body as { version_id: string }).version_id;
    expect((await load()).body).toMatchObject({ skill_pins: [skill] });
    expect(reads).toHaveLength(2);
  } finally {
    await db.drop();
  }
});
