import { deliverBudgetAlerts } from '../src/ledger/alerts.ts';
import { stepCostRoute } from '../src/ledger/step-cost.ts';
import { reserve } from '../src/ledger/reserve.ts';
import { settle } from '../src/ledger/settle.ts';
import { seedEnvironment } from '../src/resources/environments.ts';
import { ledgerRoutes } from '../src/ledger/routes.ts';
import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { scheduler } from '../src/scheduler/service.ts';
import { expect, it, vi } from 'vitest';
import { fairOrder } from '../src/scheduler/fairness.ts';
import { ReadyQueue } from '../src/scheduler/queue.ts';
import { admissionAmount, checkConcurrency, defaultLimits, liveSlot, refreshSlot, releaseSlot } from '../src/scheduler/admission.ts';
import { schedulerMetrics } from '../src/scheduler/webhook.ts';

const item = (project_id: string, index: number) => ({
  step_run_id: `${project_id}-${index}`, execution_id: 'run', project_id, agent_id: project_id,
  ready_at: new Date(index * 1000).toISOString(), max_cost_usd: 1,
});

it('admits_only_when_every_limit_has_room', () => {
  expect(checkConcurrency({ org: 1, group: 1, project: 1, agent: 1, runner: 1 },
    { org: 20, group: 10, project: 2, agent: 5, runner: 3 })).toBeUndefined();
  expect(checkConcurrency({ org: 1, group: 1, project: 2, agent: 1, runner: 1 },
    { org: 20, group: 10, project: 2, agent: 5, runner: 3 })).toBe('limit:project');
});
it('blocks the environment concurrency cap even when the other scopes have room', () => {
  expect(checkConcurrency({ org: 1, group: 1, project: 1, agent: 1, runner: 1, environment: 2 },
    { org: 20, group: 10, project: 5, agent: 5, runner: 3, environment: 2 })).toBe('limit:environment');
});
it('projects_served_round_robin', () => {
  const ordered = fairOrder([...Array.from({ length: 50 }, (_, i) => item('A', i)), item('B', 0), item('B', 1)]);
  expect(ordered.slice(0, 4).map(({ project_id }) => project_id)).toEqual(['A', 'B', 'A', 'B']);
});
it('slot_released_when_lease_lapses', () => {
  expect(liveSlot({ expiresAt: 1000 }, 999)).toBe(true);
  expect(liveSlot({ expiresAt: 1000 }, 1000)).toBe(false);
});
it('webhook_and_sweep_dedupe', () => {
  const queue = new ReadyQueue();
  queue.add([item('a', 0)]);
  queue.add([item('a', 0)]);
  expect(queue.ordered()).toHaveLength(1);
});
it('admission_reserve_is_min_of_025_and_step_cap', () => {
  expect(admissionAmount(1)).toBe(0.25);
  expect(admissionAmount(0.1)).toBe(0.1);
});
it('waiting_reason_reported_for_limited_steps', () => {
  expect(checkConcurrency({ org: 20, group: 1, project: 1, agent: 1, runner: 1 },
    { org: 20, group: 10, project: 5, agent: 5, runner: 3 })).toBe('limit:org');
});
it('defaults_are_20_concurrent_and_50_per_day', () => {
  expect(defaultLimits).toMatchObject({ orgConcurrency: 20, orgDailyUsd: 50 });
});
it('exports_queue_depth_and_running_metrics', () => {
  expect(schedulerMetrics([{ group: 'default', depth: 3 }], [{ group: 'default', runner: 'local', count: 2 }]))
    .toContain('runbook_runner_running{group="default",runner="local"} 2');
});

it.skipIf(!hasTestDatabase)('admission and release use the authenticated runner and preserve earlier slots on item failure', async () => {
  const db = await createTestDatabase();
  const pool = db.pool('scheduler');
  const now = new Date().toISOString();
  await db.admin().query(`INSERT INTO groups(group_id,name,projects) VALUES ('g','group','["p"]')`);
  await db.admin().query("INSERT INTO runners(runner_id,group_id,name,capacity) VALUES ('r','g','runner',3), ('other','g','other',3)");
  await db.admin().query("INSERT INTO agents(id,project_id,group_id,name,status,current_version_id) VALUES ('p','p','g','agent','active','v')");
  await db.admin().query('INSERT INTO agent_versions(id,agent_id,version,config,ceiling,created_by) VALUES ($1,$2,1,$3,$4,$5)',
    ['v', 'p', '{}', '{"max_cost_usd":1}', 'test']);
  const first = { ...item('p', 1), kind: 'decision', max_cost_usd: 0.1, ready_at: now };
  const bad = { ...item('p', 2), max_cost_usd: -1, ready_at: now };
  vi.stubGlobal('fetch', async () => Response.json({ items: [first, bad] }));
  const schedule = scheduler({ db: pool, coreUrl: 'https://core.example', installId: 'install', token: 'directory', orgId: 'org' });
  const [admission, release] = schedule.routes;
  try {
    const legacy = await admission!.handle({ caller: 'worker', runnerId: 'other', params: {}, body: { free_slots: 2 } });
    expect(legacy.body).toEqual({ items: [] });
    const result = await admission!.handle({
      caller: 'worker', runnerId: 'r', params: {}, body: { runner_id: 'other', free_slots: 2, decision_contract: 1 },
    });
    const { items } = result.body as { items: { step_run_id: string; slot_id: string }[] };
    expect(items).toEqual([{ step_run_id: first.step_run_id, slot_id: expect.any(String), kind: 'decision' }]);
    const reservation = await pool.query(`SELECT r.amount_usd FROM slots s
      JOIN ledger_reservations r ON r.id = s.admission_reservation_id`);
    expect(Number(reservation.rows[0]?.amount_usd)).toBe(0.1);
    const params = { slot_id: items[0]!.slot_id };
    expect((await pool.query('SELECT runner_id FROM slots')).rows).toEqual([{ runner_id: 'r' }]);
    expect(await release!.handle({ caller: 'worker', runnerId: 'other', params, body: { runner_id: 'r' } })).toEqual({ status: 404 });
    expect(await release!.handle({ caller: 'worker', runnerId: 'r', params, body: undefined })).toEqual({ status: 204 });
  } finally {
    vi.unstubAllGlobals();
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('enforces configured run and daily spend caps at admission and reserve and project concurrency at admission', async () => {
  const db = await createTestDatabase();
  const admin = db.admin();
  const pool = db.pool('scheduler');
  try {
    await admin.query(`INSERT INTO groups(group_id,name,projects) VALUES ('g','group','["p"]')`);
    await admin.query("INSERT INTO runners(runner_id,group_id,name,capacity) VALUES ('r','g','runner',3)");
    await admin.query("INSERT INTO agents(id,project_id,group_id,name,status,current_version_id) VALUES ('p','p','g','agent','active','v')");
    await admin.query(`INSERT INTO agent_versions(id,agent_id,version,config,ceiling,created_by)
      VALUES ('v','p',1,'{"limits":{"cost_per_day_usd":0.5}}','{"max_cost_usd":1}','test')`);
    await admin.query(`UPDATE policy SET run_max_cost_usd = 0.7,
      project_limits = '{"p":{"cost_per_day_usd":0.6,"concurrency":1}}'`);
    const waiting: unknown[] = [];
    vi.stubGlobal('fetch', async (url: string, init?: RequestInit) => {
      if (url.endsWith('/waiting')) waiting.push(...JSON.parse(String(init?.body)).items);
      return Response.json({ items: [item('p', 1), item('p', 2)] });
    });
    const wakeAlerts = vi.fn();
    const schedule = scheduler({ db: pool, coreUrl: 'https://core.example', installId: 'install', token: 'directory', orgId: 'org', wakeAlerts });
    const ctx = { caller: 'worker' as const, runnerId: 'r', params: {}, body: { free_slots: 2 } };
    const first = await schedule.routes[0]!.handle(ctx);
    expect(first.body).toMatchObject({ items: [{ step_run_id: 'p-1' }] });
    expect(waiting).toEqual([{ step_run_id: 'p-2', reason: 'limit:project', detail: 'project p: concurrency limit 1 reached' }]);

    const slot = (first.body as { items: { slot_id: string }[] }).items[0]!;
    const claims = { sub: 'p-1', runner_id: 'r', group_id: 'g', project_id: 'p', agent_id: 'p', agent_version: 'v',
      claim_id: 'claim', generation: 1, exp: Math.floor(Date.now() / 1000) + 600 };
    expect(await refreshSlot(pool, slot.slot_id, claims)).toBe(true);
    const verifier = { verifyCapability: async () => claims } as unknown as Parameters<typeof ledgerRoutes>[1];
    const [reserveRoute, settleRoute] = ledgerRoutes(pool, verifier, 'org', async () => {});
    const reserveCall = (amount_usd: number) => reserveRoute!.handle({
      caller: 'gateway', params: {}, body: { capability: 'test', kind: 'model', amount_usd },
    });
    expect(await reserveCall(0.8)).toEqual({ status: 402, body: { level: 'run' } });
    await admin.query(`UPDATE policy SET project_limits = jsonb_set(project_limits, '{p,cost_per_day_usd}', '0.4')`);
    expect(await reserveCall(0.45)).toEqual({ status: 402, body: { level: 'project' } });
    await admin.query(`UPDATE policy SET project_limits = jsonb_set(project_limits, '{p,cost_per_day_usd}', '0.6')`);
    expect(await reserveCall(0.55)).toEqual({ status: 402, body: { level: 'agent' } });
    await admin.query(`INSERT INTO agent_versions(id,agent_id,version,config,ceiling,created_by)
      VALUES ('narrow','p',2,'{"limits":{"cost_per_day_usd":0.5,"cost_per_month_usd":0.4}}','{"max_cost_usd":1}','test')`);
    await admin.query("UPDATE agents SET current_version_id = 'narrow' WHERE id = 'p'");
    expect(await reserveCall(0.45)).toMatchObject({ status: 402, body: { level: 'agent' } });
    await admin.query("UPDATE agents SET current_version_id = 'v' WHERE id = 'p'");
    const reserved = (await reserveCall(0.5)).body as { reservation_id: string };
    expect(reserved).toHaveProperty('reservation_id');
    const settleCall = (capability?: string) => settleRoute!.handle({
      caller: 'gateway', params: {}, body: { ...reserved, capability, actual_usd: 0.5, usage: {}, usage_pending: true },
    });
    await expect(settleCall()).rejects.toMatchObject({ code: 'ledger.invalid_request' });
    await expect(settleCall('another-claim')).rejects.toMatchObject({ code: 'ledger.reservation_denied' });
    await expect(settleCall('test')).resolves.toEqual({ status: 204 });
    const pending = await pool.query('SELECT usage_pending FROM ledger_reservations WHERE id = $1', [reserved.reservation_id]);
    expect(pending.rows).toEqual([{ usage_pending: true }]);

    const cost = await stepCostRoute(pool, verifier, () => undefined).handle({
      caller: 'worker', runnerId: 'r', params: { slot_id: slot.slot_id }, body: { capability: 'test' },
    });
    expect(cost.body).toEqual({ model_usd: 0.5, compute_usd: 0, input_tokens: 0, output_tokens: 0, cached_tokens: 0 });
    await expect(reserveCall(0.01)).rejects.toMatchObject({ code: 'ledger.slot_unavailable' });

    await schedule.routes[1]!.handle({ ...ctx, params: { slot_id: slot.slot_id } });
    expect(wakeAlerts).toHaveBeenCalledOnce();
    const outbox = await pool.query("SELECT data FROM budget_event_outbox WHERE kind = 'runner.task_cost'");
    expect(outbox.rows).toEqual([{ data: {
      step_run_id: 'p-1', claim_id: 'claim', runner_id: 'r', cost: cost.body,
    } }]);
    const next = { ...item('p', 3), execution_id: 'next-run' };
    vi.stubGlobal('fetch', async (url: string, init?: RequestInit) => {
      if (url.endsWith('/waiting')) waiting.push(...JSON.parse(String(init?.body)).items);
      return Response.json({ items: [item('p', 2), next] });
    });
    const limited = await schedule.routes[0]!.handle(ctx);
    expect(limited.body).toEqual({ items: [] });
    expect(waiting.slice(-2)).toEqual([
      { step_run_id: 'p-2', reason: 'limit:run', detail: 'run run: run budget $0.7 reached' }, { step_run_id: 'p-3', reason: 'limit:agent', detail: 'agent agent: daily budget $0.5 reached' },
    ]);
    await admin.query("UPDATE agents SET current_version_id = 'v2' WHERE id = 'p'");
    await admin.query(`INSERT INTO agent_versions(id,agent_id,version,config,ceiling,created_by)
      VALUES ('v2','p',3,'{"limits":{"cost_per_day_usd":1}}','{"max_cost_usd":1}','test')`);
    await schedule.routes[0]!.handle(ctx);
    expect(waiting.at(-1)).toEqual({ step_run_id: 'p-3', reason: 'limit:agent', detail: 'agent agent: daily budget $0.5 reached' });
  } finally {
    vi.unstubAllGlobals();
    await db.drop();
  }
});

it('loads later ready pages before ordering projects fairly', async () => {
  const db = { query: vi.fn() } as unknown as Parameters<typeof scheduler>[0]['db'];
  const fetcher = vi.fn(async (url: string) => new URL(url).searchParams.has('cursor')
    ? Response.json({ items: [item('A', 100), item('B', 101)] })
    : Response.json({ items: Array.from({ length: 100 }, (_, i) => item('A', i)), next_cursor: 'page-two' }));
  vi.stubGlobal('fetch', fetcher);
  try {
    const queue = await scheduler({ db, coreUrl: 'https://core.example', installId: 'install', token: 'directory', orgId: 'org' }).wake('g');
    expect(queue.size).toBe(102);
    expect(queue.ordered().slice(0, 3).map(({ project_id }) => project_id)).toEqual(['A', 'B', 'A']);
    expect(fetcher.mock.calls[1]?.[0]).toContain('cursor=page-two');
    fetcher.mockClear();
    fetcher.mockResolvedValueOnce(Response.json({ items: [item('A', 102)] }));
    const projectQueue = await scheduler({ db, coreUrl: 'https://core.example', installId: 'install', token: 'directory', orgId: 'org' }).wake('g', 'A');
    expect(projectQueue.ordered()).toEqual([item('A', 102)]);
    expect(fetcher.mock.calls[0]?.[0]).toContain('project=A');
  } finally {
    vi.unstubAllGlobals();
  }
});

it.skipIf(!hasTestDatabase)('settled usage for a lost decision lease is delivered once from the ledger', async () => {
  const db = await createTestDatabase();
  const pool = db.pool('scheduler');
  try {
    await db.admin().query(`INSERT INTO slots (id, step_run_id, group_id, runner_id, project_id, agent_id, claim_id, claim_generation)
      VALUES ('slot','step','g','r','p','agent','lease',1)`);
    await db.admin().query(`INSERT INTO ledger_reservations
      (id,step_run_id,runner_id,group_id,bucket_ids,kind,amount_usd,actual_usd,status,decision_lease_id)
      VALUES ('lost','step','r','g','[]','model',0.2,0.023,'settled','lease'),
             ('old','step','r','g','[]','model',0.2,0.010,'settled','older-lease')`);
    await db.admin().query(`INSERT INTO usage_events
      (step_run_id,group_id,runner_id,project_id,agent_id,kind,input_tokens,cost_usd,reservation_id)
      VALUES ('step','g','r','p','agent','model',17,0.023,'lost'),
             ('step','g','r','p','agent','model',6,0.010,'old')`);
    expect(await releaseSlot(pool, 'r', 'slot')).toBe(true);
    expect(await releaseSlot(pool, 'r', 'slot')).toBe(false);
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response(null, { status: 204 }));
    const options = { coreUrl: 'https://core.example', installId: 'install', token: 'directory', fetch: fetcher };
    await deliverBudgetAlerts(pool, options);
    await deliverBudgetAlerts(pool, options);
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(JSON.parse(String(fetcher.mock.calls[0]?.[1]?.body))).toMatchObject({
      kind: 'runner.decision_cost', data: { step_run_id: 'step', lease_id: 'lease',
        cost: { model_usd: 0.023, input_tokens: 17 } },
    });
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('requires the default environment and attributes live legacy holds before admitting work', async () => {
  const db = await createTestDatabase();
  const pool = db.pool('scheduler');
  const admin = db.admin();
  const ready = item('p', 2);
  const claims = { sub: 'p-1', runner_id: 'r', group_id: 'g', project_id: 'p', agent_id: 'p',
    agent_version: 'v', claim_id: 'legacy', generation: 1, exp: Math.floor(Date.now() / 1000) + 600 };
  const verifier = { verifyCapability: async () => claims } as unknown as Parameters<typeof ledgerRoutes>[1];
  const route = ledgerRoutes(pool, verifier, 'org', () => {}, true)[0]!;
  const compute = () => route.handle({ caller: 'sandbox-ctl', params: {},
    body: { capability: 'legacy', kind: 'compute', amount_usd: 0.1 } });
  const waiting: unknown[] = [];
  vi.stubGlobal('fetch', async (url: string, init?: RequestInit) => {
    if (url.endsWith('/waiting')) waiting.push(...JSON.parse(String(init?.body)).items);
    return Response.json({ items: [ready] });
  });
  try {
    await admin.query(`
      INSERT INTO groups(group_id,name,projects) VALUES ('g','group','["p"]');
      INSERT INTO runners(runner_id,group_id,name,capacity) VALUES ('r','g','runner',3);
      INSERT INTO agents(id,project_id,group_id,name,status,current_version_id) VALUES ('p','p','g','agent','active','v');
      INSERT INTO agent_versions(id,agent_id,version,config,ceiling,created_by) VALUES ('v','p',1,'{}','{"max_cost_usd":1}','test');
      INSERT INTO slots(id,step_run_id,runner_id,group_id,project_id,agent_id,execution_id,max_cost_usd,claim_id,claim_generation,expires_at)
      VALUES ('legacy-slot','p-1','r','g','p','p','legacy-run',5,'legacy',1,now()+interval '10 minutes');`);
    const hold = await reserve(pool, {
      orgId: 'org', groupId: 'g', projectId: 'p', agentId: 'p', runnerId: 'r', runId: 'legacy-run', stepId: 'p-1',
      timeZone: 'UTC', caps: {},
    }, 0.75, 'model');
    if (!('reservation_id' in hold)) throw new Error('legacy reservation refused');
    await pool.query("UPDATE ledger_reservations SET slot_id='legacy-slot' WHERE id=$1", [hold.reservation_id]);
    const schedule = scheduler({ db: pool, coreUrl: 'https://core.example', installId: 'install', token: 'directory',
      orgId: 'org', requireDefaultEnvironment: true });
    const admission = () => schedule.routes[0]!.handle({ caller: 'worker', runnerId: 'r', params: {}, body: { free_slots: 2 } });
    await expect(admission()).rejects.toMatchObject({ code: 'environment.default_unavailable' });
    await expect(compute()).rejects.toMatchObject({ code: 'environment.default_unavailable' });

    await seedEnvironment(db.pool('api'), 'org', 'sha256:' + 'a'.repeat(64));
    const { rows: [env] } = await admin.query<{ id: string }>('SELECT id FROM environments WHERE is_default');
    await admin.query(`INSERT INTO environment_versions(id,environment_id,version,config)
      VALUES ('v-default-2',$1,2,'{"budgets":{"cost_per_month_usd":0.8,"concurrency":1}}')`, [env!.id]);
    await expect(admission()).resolves.toEqual({ body: { items: [] } });
    expect(waiting).toContainEqual(expect.objectContaining({ step_run_id: ready.step_run_id, reason: 'limit:environment' }));
    await expect(compute()).resolves.toEqual({ status: 402, body: { level: 'environment', budget: {
      scope: 'environment', key: env!.id, period: 'month', cap: 0.8, detail: 'environment Default: monthly budget $0.8 reached',
    } } });
    await settle(pool, { ...hold, actual_usd: 0.75, usage: {} });
    expect((await pool.query("SELECT reserved_usd,spent_usd FROM ledger_buckets WHERE level='environment' ORDER BY period")).rows)
      .toEqual(Array.from({ length: 2 }, () => ({ reserved_usd: '0.000000', spent_usd: '0.750000' })));
  } finally {
    vi.unstubAllGlobals();
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('intersects live environment edits with the running pin and keeps a closed month closed', async () => {
  const db = await createTestDatabase();
  const pool = db.pool('scheduler');
  const admin = db.admin();
  try {
    await admin.query(`
      INSERT INTO environments(id,org_id,name,is_default) VALUES ('env','org','Default',true);
      INSERT INTO environment_versions(id,environment_id,version,config) VALUES ('env-1','env',1,'{"budgets":{"cost_per_month_usd":1}}');
      INSERT INTO groups(group_id,name,projects) VALUES ('g','group','["p"]');
      INSERT INTO agents(id,project_id,group_id,name,status,current_version_id) VALUES ('p','p','g','agent','active','v');
      INSERT INTO agent_versions(id,agent_id,version,config,ceiling,created_by)
        VALUES ('v','p',1,'{"environment_id":"env","environment_version_id":"env-1"}','{"max_cost_usd":5}','test');
      INSERT INTO slots(id,step_run_id,runner_id,group_id,project_id,agent_id,execution_id,max_cost_usd,claim_id,claim_generation,
          expires_at,environment_id,agent_version_id)
        VALUES ('slot','step','r','g','p','p','run',5,'claim',1,now()+interval '10 minutes','env','v');`);
    const claims = { sub: 'step', runner_id: 'r', group_id: 'g', project_id: 'p', agent_id: 'p',
      agent_version: 'v', claim_id: 'claim', generation: 1 };
    const verifier = { verifyCapability: async () => claims } as unknown as Parameters<typeof ledgerRoutes>[1];
    const route = ledgerRoutes(pool, verifier, 'org', () => {}, true)[0]!;
    const hold = async (amount_usd: number) => route.handle({ caller: 'sandbox-ctl', params: {},
      body: { capability: 'fixture', kind: 'compute', amount_usd } });
    const edit = (version: number, cap: number) => admin.query(`
      INSERT INTO environment_versions(id,environment_id,version,config) VALUES ($1,'env',$2,$3)`,
      [`env-${version}`, version, JSON.stringify({ budgets: { cost_per_month_usd: cap } })]);
    const first = (await hold(0.7)).body as { reservation_id: string };
    await edit(2, 2);
    expect(await hold(0.31)).toMatchObject({ status: 402, body: { budget: { cap: 1, period: 'month' } } });
    await edit(3, 0.5);
    expect(await hold(0.01)).toMatchObject({ status: 402, body: { budget: { cap: 0.5, period: 'month' } } });
    await settle(pool, { ...first, actual_usd: 0.7, usage: {} });
    // Admission observes the lower cap against settled spend.
    expect(await hold(0.01)).toMatchObject({ status: 402, body: { budget: { cap: 0.5, period: 'month' } } });
    await edit(4, 3);
    expect(await hold(0.01)).toMatchObject({ status: 402, body: { budget: { cap: 0.5, period: 'month' } } });
    expect((await pool.query("SELECT reserved_usd,spent_usd FROM ledger_buckets WHERE level='environment' AND period='month'")).rows)
      .toEqual([{ reserved_usd: '0.000000', spent_usd: '0.700000' }]);
  } finally {
    await db.drop();
  }
});
