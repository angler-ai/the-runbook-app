import { expect, it } from 'vitest';
import type { Environment } from '@runbook/runner-shared/contracts/environments';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { environmentConfig, seedEnvironment } from '../src/resources/environments.ts';
import { publicResources } from '../src/resources/public.ts';
import { gatewayResources } from '../src/resources/gateway.ts';
import { reserve } from '../src/ledger/reserve.ts';
import { settle } from '../src/ledger/settle.ts';
import type { Budget } from '@runbook/runner-shared/contracts/budgets';

it.each(['PATH', 'HOME', 'HTTP_PROXY', 'NODE_OPTIONS', 'RUNBOOK_TEST', 'bad_name'])('refuses %s', (name) => {
  expect(() => environmentConfig({ env: { [name]: 'fixture' } })).toThrow('invalid or reserved');
});

it('checks secret shape, collisions and setup limits', () => {
  const fixture = 'ghp_' + 'X'.repeat(20);
  expect(() => environmentConfig({ env: { MY_TOKEN: fixture } })).toThrow('use a credential');
  expect(() => environmentConfig({ env: { TOKEN: 'placeholder' }, credential_env: { TOKEN: 'id' } })).toThrow('already defined');
  expect(() => environmentConfig({ setup_timeout_s: 601 })).toThrow('between 1 and 600');
  expect(environmentConfig({}).setup_timeout_s).toBe(120);
});

const claims = {
  session_hash: 'browser', org_id: 'org', org_role: 'owner', principal: 'human', scopes: ['runner:read', 'runner:admin'],
  project_roles: { p: 'admin' }, user_id: 'owner',
} as unknown as UserAssertionClaims;

it.skipIf(!hasTestDatabase)('audits environment writes and uses pinned digests, limits and placeholder bindings', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    const admin = db.admin();
    const resolver = publicResources(pool, {} as Parameters<typeof publicResources>[1], async () => {});
    const call = (method: string, path: string, body?: unknown, actor = claims) =>
      resolver({ method, target: `/runner/v1${path}`, claims: actor, body });
    const digest = (value: string) => `registry.example/sandbox@sha256:${value.repeat(64)}`;
    await seedEnvironment(pool, 'org', `sha256:${'0'.repeat(64)}`);
    const initial = (await call('GET', '/environments')).body as { items: Environment[] };
    expect(initial.items[0]).toMatchObject({ is_default: true, image_digest: `sha256:${'0'.repeat(64)}`, version: 1 });
    await expect(call('DELETE', `/environments/${initial.items[0]!.id}`)).rejects.toMatchObject({ status: 409 });
    await expect(call('POST', '/environments', { name: 'Data', image: digest('1') })).rejects.toMatchObject({
      code: 'environment.registry_not_allowed',
    });
    await expect(call('PUT', '/environment-registries', { items: ['registry.example'] }, { ...claims, org_role: 'admin' }))
      .rejects.toMatchObject({ status: 403 });
    await expect(call('PUT', '/environment-registries', { items: [] }, { ...claims, session_hash: undefined }))
      .rejects.toMatchObject({ status: 403 });
    await expect(call('GET', '/environment-registries')).resolves.toEqual({ body: { items: [] } });
    await call('PUT', '/environment-registries', { items: ['registry.example'] });
    await admin.query(`INSERT INTO groups(group_id,name,projects) VALUES ('g','Group','["p"]');
      INSERT INTO credentials(id,org_id,name,type,config,delivery,status,sharing,label) VALUES
        ('http','org','API key','api-key','{"hosts":["api.example"]}','["proxy"]','active','{"projects":"all"}','read'),
        ('model','org','Model key','model-provider','{}','["model"]','active','{"projects":"all"}','read');`);
    await expect(call('POST', '/environments', { name: 'Data', credential_env: { TOKEN: 'model' } })).rejects.toMatchObject({
      code: 'environment.invalid', message: expect.stringContaining('proxy credential'),
    });
    const response = await call('POST', '/environments', { name: 'Data', image: digest('1'), setup: 'printf prepared',
      env: { REGION: 'us' }, credential_env: { TOKEN: 'http' }, size: 'medium', timeout_s: 20,
      egress: [{ host: 'api.example', label: 'read' }], budgets: { max_step_cost_usd: 0.5, max_run_cost_usd: 2 } });
    expect(response.status).toBe(201);
    const environment = response.body as Environment;
    expect((await call('GET', `/environments/${environment.id}`)).body).toEqual(environment);
    const agent = { name: 'data', group_id: 'g', environment_id: environment.id, status: 'active',
      credentials: [{ credential_id: 'http', delivery: 'proxy' }] };
    await expect(call('POST', '/projects/p/agents', { ...agent, limits: { max_cost_usd: 1, max_tokens: 100, timeout_s: 20 } }))
      .rejects.toMatchObject({ code: 'agent.environment_limit' });
    await expect(call('POST', '/projects/p/agents', { ...agent, egress: [{ host: 'api.example', label: 'write' }] }))
      .rejects.toMatchObject({ code: 'agent.environment_limit' });
    const saved = (await call('POST', '/projects/p/agents', agent)).body as { id: string; version_id: string };
    const agentRead = (await call('GET', `/projects/p/agents/${saved.id}`)).body;
    expect(agentRead).toMatchObject({ config: { environment_version_id: environment.version_id, sandbox: { size: 'medium' },
      limits: { timeout_s: 20, max_cost_usd: 0.5, max_run_cost_usd: 2 } } });
    await call('PATCH', `/environments/${environment.id}`, { image: digest('2') });
    expect((await call('POST', `/environments/${environment.id}/resolve-digest`)).body).toMatchObject({ version: 3, image_digest: digest('2') });

    const grant = { agent_version: saved.version_id, agent_id: saved.id, project_id: 'p', group_id: 'g', connections: ['http'] };
    const verifier = { verifyCapability: async () => grant } as unknown as Parameters<typeof gatewayResources>[1];
    const route = gatewayResources(pool, verifier, 'org').find(({ path }) => path === '/internal/environment/sandbox')!;
    const sandbox = () => route.handle({ caller: 'sandbox-ctl', body: { capability: 'test' }, params: {} });
    expect((await sandbox()).body).toMatchObject({
      image: digest('1'), credential_names: ['TOKEN'], setup: 'printf prepared', env: { REGION: 'us' },
    });
    await call('DELETE', `/environments/${environment.id}`);
    await expect(call('GET', `/environments/${environment.id}`)).rejects.toMatchObject({ status: 404 });
    await expect(sandbox()).resolves.toMatchObject({ body: { image: digest('1') } });
    await call('PUT', '/environment-registries', { items: [] });
    await expect(sandbox()).rejects.toMatchObject({ code: 'sandbox.image_not_approved' });
    expect((await pool.query("SELECT action FROM audit_log WHERE resource_type='environment' ORDER BY seq")).rows.map(({ action }) => action))
      .toEqual(['environment.seeded', 'environment.registries_updated', 'environment.created',
        'environment.updated', 'environment.digest_resolved', 'environment.deleted', 'environment.registries_updated']);
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('enforces environment and monthly caps, with one configured alert and overview history', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    const scheduler = db.pool('scheduler');
    const api = publicResources(pool, {} as Parameters<typeof publicResources>[1], async () => {});
    await seedEnvironment(pool, 'org', `sha256:${'0'.repeat(64)}`);
    const created = await api({
      method: 'POST', target: '/runner/v1/environments', claims,
      body: { name: 'Budgeted', budgets: { cost_per_day_usd: 1, cost_per_month_usd: 2, concurrency: 2, alert_threshold_percent: 25 } },
    });
    const { id } = created.body as Environment;
    const context = { orgId: 'org', groupId: 'g', projectId: 'p', agentId: 'a', environmentId: id, runnerId: 'r',
      timeZone: 'UTC', stepId: 'step', runId: 'run', caps: { 'environment:day': 1, 'environment:month': 2,
        'agent:month': 1.2, 'project:month': 1.3 },
      alerts: { environment: 25 } } as const;
    for (const amount of [0.5, 0.25]) {
      const hold = await reserve(scheduler, context, amount, 'model');
      if (!('reservation_id' in hold)) throw new Error('reservation failed');
      const alerts = await settle(scheduler, { reservation_id: hold.reservation_id, actual_usd: amount, usage: {} });
      expect(alerts.filter((event) => event.level === 'environment')).toHaveLength(amount === 0.5 ? 2 : 0);
    }
    await expect(reserve(scheduler, context, 0.3, 'model')).resolves.toEqual({ level: 'environment' });
    await expect(reserve(scheduler, { ...context, caps: { ...context.caps, 'environment:day': 2 } }, 0.5, 'model'))
      .resolves.toEqual({ level: 'agent' });
    await db.admin().query(`INSERT INTO budget_waiting(step_run_id,group_id,project_id,scope,budget_key,period,detail,execution_id)
      VALUES ('blocked','g','p','environment',$1,'day','environment Budgeted: daily budget $1 reached','exe_budgeted')`, [id]);
    const { body } = await api({ method: 'GET', target: '/runner/v1/budgets', claims, body: undefined });
    const { items } = body as { items: Budget[] };
    const daily = items.find((row) => row.id === id && row.period === 'day')!;
    expect(daily).toMatchObject({ scope: 'environment', cap: 1, spend: 0.75, percentage: 75,
      alert_threshold_percent: 25, blocked_count: 1, blocked: [{ step_run_id: 'blocked', execution_id: 'exe_budgeted', reason: expect.stringContaining('Budgeted') }] });
    expect(daily.daily_spend).toHaveLength(30);
    expect(daily.daily_spend.at(-1)?.spend_usd).toBe(0.75);
    expect(items.find((row) => row.id === id && row.period === 'concurrency')).toMatchObject({ cap: 2, spend: 0 });
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('paginates only visible budget rows and includes a waiting-only run', async () => {
  const db = await createTestDatabase();
  try {
    await db.admin().query(`INSERT INTO budget_waiting(step_run_id,group_id,project_id,scope,budget_key,period,detail,cap)
      VALUES ('step-a','g','p','run','run-a','run','run-a is blocked',0.1),
             ('step-b','g','p','run','run-b','run','run-b is blocked',0.1),
             ('step-c','g','private','run','run-c','run','private',0.1)`);
    const api = publicResources(db.pool('api'), {} as Parameters<typeof publicResources>[1], async () => {});
    const read = async (query: string) => {
      const target = '/runner/v1/budgets?' + query;
      const { body } = await api({ method: 'GET', target, claims: { ...claims, htu: target, org_role: 'member' }, body: undefined });
      return body as { items: Budget[]; next_cursor: string | null };
    };
    const first = await read('limit=1');
    expect(first.items).toEqual([expect.objectContaining({ id: 'run-a', spend: 0, cap: 0.1,
      blocked_count: 1, execution_id: 'run-a',
      blocked: [{ step_run_id: 'step-a', reason: 'run-a is blocked', execution_id: 'run-a' }] })]);
    expect(first.items[0]?.daily_spend).toHaveLength(30);
    const second = await read('limit=1&cursor=' + first.next_cursor);
    expect(second.items).toEqual([expect.objectContaining({ id: 'run-b' })]);
    expect(second.next_cursor).toBeNull();
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('counts all visible budget waits before limiting linked details', async () => {
  const db = await createTestDatabase();
  try {
    await db.admin().query(`INSERT INTO budget_waiting(step_run_id,group_id,project_id,scope,budget_key,period,detail,cap,execution_id)
      SELECT 'step-' || n, 'g', 'p', 'run', 'run-a', 'run', 'budget reached', 1, 'run-a'
      FROM generate_series(1,26) n`);
    const api = publicResources(db.pool('api'), {} as Parameters<typeof publicResources>[1], async () => {});
    const target = '/runner/v1/budgets?scope=run&blocked=1';
    const { body } = await api({ method: 'GET', target, claims: { ...claims, htu: target, org_role: 'member' }, body: undefined });
    const { items } = body as { items: Budget[] };
    expect(items).toHaveLength(1);
    expect(items[0]?.blocked_count).toBe(26);
    expect(items[0]?.blocked).toHaveLength(25);
    expect(items[0]?.blocked[0]).toEqual({ step_run_id: 'step-1', execution_id: 'run-a', reason: 'budget reached' });
  } finally {
    await db.drop();
  }
});
