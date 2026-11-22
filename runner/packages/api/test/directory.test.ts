import { publicResources } from '../src/resources/public.ts';
import { scheduler } from '../src/scheduler/service.ts';
import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { saveAgentVersion } from '../src/resources/agents.ts';
import type { AgentDirectoryEntry } from '@runbook/runner-shared/contracts/core';
import { syncAgent, syncDirectory } from '../src/directory-sync.ts';
import { expect, it, vi } from 'vitest';
import { checkCoreContract } from '../src/registration.ts';

it.each([undefined, 1, 2, 2.5, 3])('refuses unsupported core contract version %s', async (version) => {
  const core = vi.fn<typeof fetch>(async () => Response.json({ runner_contract_version: version }));
  await expect(checkCoreContract('https://core.example', core)).rejects.toThrow('unsupported core runner contract');
  expect(core).toHaveBeenCalledWith('https://core.example/v1/meta', expect.anything());
});

it.each([4, 5])('accepts core contract version %s from meta', async (version) => {
  const core = vi.fn<typeof fetch>(async () => Response.json({ runner_contract_version: version }));
  await expect(checkCoreContract('https://core.example', core)).resolves.toBeUndefined();
});

it('directory_synced_on_version_change', async () => {
  const fetcher = vi.fn<typeof fetch>(async () => new Response(null, { status: 204 }));
  const entry: AgentDirectoryEntry = {
    agent_id: 'a', project_id: 'p', group_id: 'g', version_id: 'second', name: 'agent', status: 'active',
    ceiling: { profiles: [], max_cost_usd: 1, max_tokens: 100, timeout_s: 60, credentials: [], tools: [], hosts: [] },
  };
  await syncAgent({ coreUrl: 'https://core.example', installId: 'install', token: 'fixture', fetch: fetcher }, entry);
  expect(fetcher).toHaveBeenCalledWith('https://core.example/v1/runner-installs/install/agents/a',
    expect.objectContaining({ method: 'PUT', body: JSON.stringify(entry) }));
});

it.skipIf(!hasTestDatabase)('directory recomputes credential labels from the live catalog', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    await db.admin().query("INSERT INTO credentials (id, org_id, name, type, label, sharing, delivery) VALUES ('c','org','key','api-key','read','{\"projects\":[\"p\"]}','[\"proxy\"]')");
    await pool.query("INSERT INTO agents (id, project_id, group_id, name, status) VALUES ('a','p','g','agent','active')");
    await saveAgentVersion(pool, {
      agentId: 'a', projectId: 'p', actorId: 'owner',
      config: { profiles: [], credentials: [{ credential_id: 'c', delivery: 'proxy' }],
        limits: { max_cost_usd: 1, max_tokens: 100, timeout_s: 60 } },
      ceiling: { profiles: [], credentials: [{ id: 'c', name: 'key', label: 'read', delivery: ['proxy'] }],
        tools: [], hosts: [], max_cost_usd: 1, max_tokens: 100, timeout_s: 60 },
    });
    await pool.query("INSERT INTO groups(group_id,name) VALUES ('g','group')");
    const fetcher = vi.fn<typeof fetch>(async (input) => String(input).endsWith('/groups')
      ? Response.json({ install_id: 'install', groups: [{ id: 'g', projects: ['p'] }] })
      : new Response(null, { status: 204 }));
    const sync = () => syncDirectory(pool, { coreUrl: 'https://core.example', installId: 'install',
      token: 'test', orgId: 'org', fetch: fetcher });
    await sync();
    expect(fetcher).toHaveBeenCalledWith('https://core.example/v1/runner-installs/install/groups',
      expect.objectContaining({ headers: { authorization: 'Bearer test' } }));
    await db.admin().query("UPDATE credentials SET label = 'destructive' WHERE id = 'c'");
    await sync();
    const last = fetcher.mock.lastCall?.[1]?.body as string;
    expect(JSON.parse(last)).toMatchObject({ ceiling: { credentials: [{ id: 'c', label: 'destructive' }] } });
    await db.pool('vault').query("UPDATE credentials SET sharing = '{\"projects\":\"none\"}' WHERE id = 'c'");
    await sync();
    expect(JSON.parse(fetcher.mock.lastCall?.[1]?.body as string)).toMatchObject({ ceiling: { credentials: [] } });
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('mirrors core project removals into the agent list and scheduler admission', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    await pool.query(`INSERT INTO groups(group_id,name,projects) VALUES ('g','group','["p"]'), ('old','old','["p"]');
      INSERT INTO runners(runner_id,group_id,name,capacity) VALUES ('r','g','runner',1);
      INSERT INTO agents(id,project_id,group_id,name,status,current_version_id) VALUES ('a','p','g','agent','active','v');
      INSERT INTO agent_versions(id,agent_id,version,config,ceiling,created_by)
      VALUES ('v','a',1,'{"profiles":[],"credentials":[],"limits":{"max_cost_usd":1,"max_tokens":100,"timeout_s":60}}',
        '{"max_cost_usd":1}','test')`);
    let projects = ['p'];
    let status = 200;
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const url = new URL(String(input));
      if (url.pathname.endsWith('/groups')) {
        expect(init?.headers).toMatchObject({ authorization: 'Bearer directory' });
        return Response.json({ install_id: 'install', groups: [{ id: 'g', projects }] }, { status });
      }
      if (url.pathname.endsWith('/ready')) return Response.json({
        items: [{ project_id: 'p', agent_id: 'a', step_run_id: 'step', execution_id: 'run', ready_at: 0 }],
      });
      return new Response(null, { status: 204 });
    });
    vi.stubGlobal('fetch', fetcher);
    const options = { coreUrl: 'https://core.example', installId: 'install', token: 'directory', orgId: 'org' };
    const registry = { ...options, fetch: fetcher };
    const sync = () => syncDirectory(pool, registry);
    const handle = publicResources(pool, { role: 'api', listen: { host: '', port: 0 }, urls: {},
      inboundTokens: {}, outboundTokens: {} }, sync);
    const claims = {
      iss: 'core', aud: 'runner:install', jti: 'once', iat: 0, exp: 60, org_id: 'org', user_id: 'owner',
      org_role: 'owner', principal: 'human' as const, project_roles: { p: 'admin' },
      scopes: ['runner:read', 'runner:admin'], htm: 'GET', htu: '', bh: '',
    } satisfies Parameters<typeof handle>[0]['claims'];
    const list = () => handle({ method: 'GET', target: '/runner/v1/projects/p/agents', claims, body: undefined });
    const detail = () => handle({ method: 'GET', target: '/runner/v1/projects/p/agents/a', claims, body: undefined });
    const schedule = scheduler({ ...options, db: db.pool('scheduler') });
    const admit = () => schedule.routes[0]!.handle({ caller: 'worker', runnerId: 'r', params: {}, body: { free_slots: 1 } });
    await sync();
    expect((await list()).body).toMatchObject({ items: [{ id: 'a', status: 'active' }] });
    projects = [];
    status = 503;
    await expect(sync()).rejects.toThrow();
    expect((await detail()).body).toMatchObject({ status: 'active' });
    status = 200;
    await sync();
    expect((await list()).body).toMatchObject({ items: [{ id: 'a', status: 'paused' }] });
    expect((await detail()).body).toMatchObject({ status: 'paused' });
    expect((await admit()).body).toEqual({ items: [] });
    expect((await pool.query('SELECT projects FROM groups ORDER BY group_id')).rows).toEqual([{ projects: [] }, { projects: [] }]);
    await handle({ method: 'PATCH', target: '/runner/v1/groups/g', claims, body: { projects: ['p'] } });
    expect((await detail()).body).toMatchObject({ status: 'paused' });
    projects = ['p'];
    await sync();
    expect((await detail()).body).toMatchObject({ status: 'active' });
    expect((await admit()).body).toMatchObject({ items: [{ step_run_id: 'step' }] });
  } finally {
    vi.unstubAllGlobals();
    await db.drop();
  }
});
