import * as admissionModule from '../src/scheduler/admission.ts';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { createTestDatabase, hasTestDatabase, type TestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { withTransaction } from '@runbook/runner-shared/db';
import { budgetBuckets, type BudgetContext } from '../src/ledger/buckets.ts';
import { reserve, reserveInTransaction } from '../src/ledger/reserve.ts';
import { admit, reapSlots, releaseSlot } from '../src/scheduler/admission.ts';
import { reconcile, settle } from '../src/ledger/settle.ts';
import { deliverBudgetAlerts } from '../src/ledger/alerts.ts';
import { ledgerRoutes } from '../src/ledger/routes.ts';

const context = (id: string): BudgetContext => ({
  orgId: id, groupId: id, projectId: id, agentId: id, runnerId: id, runId: id, stepId: id,
  timeZone: 'UTC', caps: {},
});

it('day_resets_in_org_time_zone', () => {
  const buckets = (iso: string) => budgetBuckets({ ...context('tz'), timeZone: 'America/Los_Angeles' }, new Date(iso));
  expect(buckets('2026-11-20T07:59:00Z').find(({ level, period }) => level === 'org' && period === 'day')?.start)
    .toBe('2026-11-19');
  expect(buckets('2026-11-20T08:00:00Z').find(({ level, period }) => level === 'org' && period === 'day')?.start)
    .toBe('2026-11-20');
});

describe.skipIf(!hasTestDatabase)('ledger (Postgres)', () => {
  let db: TestDatabase;
  beforeAll(async () => { db = await createTestDatabase(); });
  afterAll(async () => { await db?.drop(); });
  it.each([0.25, 1])('first model call can reserve the full %s step cap', async (cap) => {
    const pool = db.pool('scheduler');
    const ctx = { ...context(`admission-${cap}`), caps: { step: cap } };
    const admission = await reserve(pool, ctx, Math.min(0.25, cap), 'admission');
    expect(admission).toHaveProperty('reservation_id');
    expect(await reserve(pool, ctx, cap, 'model')).toHaveProperty('reservation_id');
    expect(await reserve(pool, ctx, 0.01, 'model')).toEqual({ level: 'step' });
  });
  it.each(['release', 'reap'])('settles outstanding model usage at reserved amount on slot %s', async (cleanup) => {
    const pool = db.pool('scheduler');
    const ctx = { ...context(cleanup), caps: { step: 1 } };
    const now = new Date();
    const deadline = new Date(now.getTime() + 600_000);
    const slot = await admit(pool, {
      step_run_id: ctx.stepId, execution_id: ctx.runId, project_id: ctx.projectId,
      agent_id: ctx.agentId, ready_at: now.toISOString(), max_cost_usd: 1,
    }, ctx, { org: 20, group: null, project: null, agent: 5, runner: 1 }, deadline, now);
    if (!slot || !('slot_id' in slot)) throw new Error('admission failed');
    const model = await reserve(pool, ctx, 0.5, 'model', now);
    if (!('reservation_id' in model)) throw new Error('reservation refused');
    await pool.query('UPDATE ledger_reservations SET slot_id = $1 WHERE id = $2',
      [slot.slot_id, model.reservation_id]);

    if (cleanup === 'reap') expect(await reapSlots(pool, deadline)).toBe(1);
    else expect(await releaseSlot(pool, ctx.runnerId, slot.slot_id)).toBe(true);

    expect((await pool.query('SELECT status, actual_usd FROM ledger_reservations WHERE id = $1',
      [model.reservation_id])).rows).toEqual([{ status: 'settled', actual_usd: '0.500000' }]);
    expect((await pool.query('SELECT kind, cost_usd FROM usage_events WHERE step_run_id = $1',
      [ctx.stepId])).rows).toEqual([{ kind: 'model', cost_usd: '0.500000' }]);
  });
  it('keeps a spent monthly budget closed when a later policy raises the cap', async () => {
    const pool = db.pool('scheduler');
    const ctx = context('closed-month');
    const hold = await reserve(pool, { ...ctx, caps: { 'agent:month': 1 } }, 1, 'model');
    if (!('reservation_id' in hold)) throw new Error('reservation refused');
    await settle(pool, { reservation_id: hold.reservation_id, actual_usd: 1, usage: {} });
    expect(await reserve(pool, { ...ctx, caps: { 'agent:month': 2 } }, 0.01, 'model')).toEqual({ level: 'agent' });
  });
  it('tracks unavoidable compute overrun without charging it against budget holds', async () => {
    const pool = db.pool('scheduler');
    const ctx = { ...context('compute-overrun'), caps: { 'project:month': 1 } };
    const hold = await reserve(pool, ctx, 1, 'compute');
    if (!('reservation_id' in hold)) throw new Error('reservation refused');
    const request = { reservation_id: hold.reservation_id, actual_usd: 1.25, usage: {} };
    await settle(pool, request);
    await settle(pool, request);
    expect((await pool.query("SELECT spent_usd FROM ledger_buckets WHERE level='project' AND period='month' AND key=$1", [ctx.projectId])).rows)
      .toEqual([{ spent_usd: '1.000000' }]);
    expect((await pool.query('SELECT actual_usd,overrun_usd FROM ledger_reservations WHERE id=$1', [hold.reservation_id])).rows)
      .toEqual([{ actual_usd: '1.250000', overrun_usd: '0.250000' }]);
  });
  it('retries a project threshold with its original occurrence and audience', async () => {
    const eventsDb = await createTestDatabase();
    const pool = eventsDb.pool('scheduler');
    try {
      const ctx = { ...context('project-alert'), caps: { 'project:month': 1 }, alerts: { project: 50 } };
      const hold = await reserve(pool, ctx, 0.75, 'model');
      if (!('reservation_id' in hold)) throw new Error('reservation refused');
      await settle(pool, { reservation_id: hold.reservation_id, actual_usd: 0.75, usage: {} });
      const events: unknown[] = [];
      const options = { coreUrl: 'https://core.example', installId: 'install', token: 'directory',
        fetch: (async (_url: string, init: RequestInit) => {
          events.push(JSON.parse(String(init.body)));
          return new Response(null, { status: events.length === 1 ? 503 : 204 });
        }) as typeof fetch };
      const now = Date.now() + 1000;
      await deliverBudgetAlerts(pool, options, new Date(now));
      await deliverBudgetAlerts(pool, options, new Date(now + 30_001));
      expect(events).toEqual([expect.objectContaining({ data: expect.objectContaining({
        project_id: ctx.projectId, occurred_at_ms: expect.any(Number), event_id: expect.any(String),
      }) }), events[0]]);
    } finally {
      await eventsDb.drop();
    }
  });
  it('reserve_checks_every_level_in_one_transaction', async () => {
    const pool = db.pool('scheduler');
    const ctx = { ...context('all'), caps: { step: 5, 'project:day': 1 } };
    expect(await reserve(pool, ctx, 2, 'model')).toEqual({ level: 'project' });
    expect((await pool.query('SELECT id FROM ledger_buckets WHERE key = $1', ['all'])).rowCount).toBe(0);
  });
  it('clamps a model hold across caps and leaves room for the final turn', async () => {
    const pool = db.pool('scheduler');
    const ctx = { ...context('clamp'), caps: { step: 1, 'project:day': 0.9 } };
    const hold = await withTransaction(pool, (client) => reserveInTransaction(client, ctx, 1, 'model', new Date(), 0.05, 0.1));
    expect(hold.amount_usd).toBe(0.8);
    await settle(pool, { reservation_id: hold.reservation_id, actual_usd: 0.8,
      usage: { input_tokens: 20, output_tokens: 2, cached_tokens: 8, cache_write_tokens: 5 } });
    await expect(withTransaction(pool, (client) => reserveInTransaction(client, ctx, 1, 'model', new Date(), 0.05, 0.1)))
      .rejects.toMatchObject({ level: 'project' });
    const final = await withTransaction(pool, (client) => reserveInTransaction(client, ctx, 1, 'model', new Date(), 0.05));
    expect(final.amount_usd).toBe(0.1);
    expect((await pool.query('SELECT cached_tokens, cache_write_tokens FROM usage_events WHERE step_run_id = $1', [ctx.stepId])).rows)
      .toEqual([{ cached_tokens: '8', cache_write_tokens: '5' }]);
  });
  it('accepts authenticated legacy settlement for reservations that predate capability hashes', async () => {
    const pool = db.pool('scheduler');
    const ctx = context('legacy-reservation');
    const hold = await reserve(pool, ctx, 1, 'model');
    if (!('reservation_id' in hold)) throw new Error('reservation refused');
    const route = ledgerRoutes(pool, {} as Parameters<typeof ledgerRoutes>[1], 'org', () => {})
      .find(({ path }) => path === '/internal/ledger/settle')!;
    const body = { reservation_id: hold.reservation_id, actual_usd: 0.25, usage: { input_tokens: 40 } };
    await expect(route.handle({ caller: 'sandbox-ctl', params: {}, body })).rejects.toMatchObject({ status: 404 });
    await expect(route.handle({ caller: 'gateway', params: {}, body })).resolves.toEqual({ status: 204 });
    await expect(route.handle({ caller: 'gateway', params: {}, body })).resolves.toEqual({ status: 204 });
    expect((await pool.query('SELECT input_tokens, cost_usd FROM usage_events WHERE step_run_id = $1', [ctx.stepId])).rows)
      .toEqual([{ input_tokens: '40', cost_usd: '0.250000' }]);
  });
  it('reconciles a pending model charge exactly once as a local operator', async () => {
    const pool = db.pool('scheduler');
    const ctx = context('reconcile-reservation');
    const hold = await reserve(pool, ctx, 1, 'model');
    if (!('reservation_id' in hold)) throw new Error('reservation refused');
    await settle(pool, { reservation_id: hold.reservation_id, actual_usd: 1, usage_pending: true, usage: {} });
    expect(ledgerRoutes(pool, {} as Parameters<typeof ledgerRoutes>[1], 'org', () => {})
      .some(({ path }) => path === '/internal/ledger/reconcile')).toBe(false);
    const adjust = (stepId: string, input_tokens = 42) => reconcile(
      pool, { reservation_id: hold.reservation_id, actual_usd: 0.25, usage: { input_tokens } },
      { stepId, runnerId: ctx.runnerId },
    );
    await expect(adjust('other-step')).rejects.toMatchObject({ status: 403 });
    await expect(adjust(ctx.stepId)).resolves.toEqual([]);
    await expect(adjust(ctx.stepId)).resolves.toEqual([]);
    await expect(adjust(ctx.stepId, 43)).rejects.toMatchObject({ status: 409 });
    expect((await pool.query('SELECT input_tokens, cost_usd FROM usage_events WHERE step_run_id = $1', [ctx.stepId])).rows)
      .toEqual([{ input_tokens: '42', cost_usd: '0.250000' }]);
    expect((await pool.query("SELECT spent_usd FROM ledger_buckets WHERE level = 'step' AND key = $1", [ctx.stepId])).rows)
      .toEqual([{ spent_usd: '0.250000' }]);
  });
  it('requires an audited override above the hold and limits scheduler usage writes', async () => {
    const pool = db.pool('scheduler');
    const ctx = context('override-reservation');
    const hold = await reserve(pool, ctx, 1, 'model');
    if (!('reservation_id' in hold)) throw new Error('reservation refused');
    const reservation_id = hold.reservation_id;
    await settle(pool, { reservation_id, actual_usd: 1, usage_pending: true, usage: {} });
    const adjustment = { reservation_id, actual_usd: 1.25, usage: { input_tokens: 42 } };
    const operator = { stepId: ctx.stepId, runnerId: ctx.runnerId };
    await expect(reconcile(pool, adjustment, operator)).rejects.toMatchObject({ code: 'ledger.above_hold' });
    await expect(pool.query('UPDATE usage_events SET cost_usd = 0 WHERE reservation_id = $1', [reservation_id]))
      .rejects.toMatchObject({ code: '42501' });
    await expect(pool.query('SELECT reconcile_pending_usage($1,1.25,0,0,0,0,false)', [reservation_id]))
      .resolves.toMatchObject({ rows: [{ reconcile_pending_usage: false }] });
    await reconcile(pool, adjustment, { ...operator, overrideHold: true });
    expect((await db.admin().query('SELECT detail FROM audit_log WHERE resource_id = $1', [reservation_id])).rows)
      .toContainEqual({ detail: expect.objectContaining({ override_hold: true }) });
    await expect(pool.query('SELECT reconcile_pending_usage($1,0,0,0,0,0,true)', [reservation_id]))
      .resolves.toMatchObject({ rows: [{ reconcile_pending_usage: false }] });
  });

  it('lets the local ledger operator reconcile a legacy provisional charge only in the matching step and runner', async () => {
    const pool = db.pool('scheduler');
    const ctx = context('legacy-pending');
    const hold = await reserve(pool, ctx, 1, 'model');
    if (!('reservation_id' in hold)) throw new Error('reservation refused');
    const request = { reservation_id: hold.reservation_id, actual_usd: 1, usage: { input_tokens: 42 } };
    await settle(pool, { ...request, usage: {}, usage_pending: true });
    await expect(reconcile(pool, request, { stepId: 'other-step', runnerId: ctx.runnerId }))
      .rejects.toMatchObject({ status: 403 });
    const scope = { stepId: ctx.stepId, runnerId: ctx.runnerId };
    await reconcile(pool, request, scope);
    await reconcile(pool, request, scope);
    expect((await pool.query('SELECT input_tokens, cost_usd FROM usage_events WHERE step_run_id = $1', [ctx.stepId])).rows)
      .toEqual([{ input_tokens: '42', cost_usd: '1.000000' }]);
  });
  it('settle_releases_unused_reservation', async () => {
    const pool = db.pool('scheduler');
    const ctx = { ...context('unused'), caps: { step: 5 } };
    const result = await reserve(pool, ctx, 5, 'model');
    if (!('reservation_id' in result)) throw new Error('reservation refused');
    await settle(pool, { reservation_id: result.reservation_id, actual_usd: 2, usage: { input_tokens: 3 } });
    expect(await reserve(pool, ctx, 3, 'model')).toHaveProperty('reservation_id');
  });
  it('concurrent_reservations_never_exceed_cap', async () => {
    const pool = db.pool('scheduler');
    const ctx = { ...context('race'), caps: { 'org:day': 50 } };
    const results = await Promise.all(Array.from({ length: 100 }, (_, index) =>
      reserve(pool, { ...ctx, stepId: `race-${index}` }, 1, 'model')));
    expect(results.filter((result) => 'reservation_id' in result)).toHaveLength(50);
    expect(results.filter((result) => 'level' in result)).toEqual(Array.from({ length: 50 }, () => ({ level: 'org' })));
  });
  it('bootstraps shared day and month buckets during simultaneous first reservations', async () => {
    const pool = db.pool('scheduler');
    const now = new Date('2026-11-20T12:00:00Z');
    for (let wave = 0; wave < 50; wave++) {
      const id = `bootstrap-${wave}`;
      const ctx = context(id);
      const results = await Promise.all(Array.from({ length: 10 }, (_, step) =>
        reserve(pool, { ...ctx, stepId: `${id}-${step}` }, 1, 'model', now)));
      expect(results.every((result) => 'reservation_id' in result)).toBe(true);
      expect((await pool.query('SELECT reserved_usd FROM ledger_buckets WHERE key = $1 AND level = $2 ORDER BY period',
        [id, 'org'])).rows).toEqual([{ reserved_usd: '10.000000' }, { reserved_usd: '10.000000' }]);
    }
  });
  it('compute_cost_counts_toward_budgets', async () => {
    const pool = db.pool('scheduler');
    const ctx = { ...context('compute'), caps: { 'org:day': 4 } };
    const result = await reserve(pool, ctx, 3, 'compute');
    if (!('reservation_id' in result)) throw new Error('reservation refused');
    await settle(pool, { reservation_id: result.reservation_id, actual_usd: 3, usage: {} });
    expect(await reserve(pool, ctx, 2, 'model')).toEqual({ level: 'org' });
  });
  it('threshold_alerts_fire_once_each', async () => {
    const pool = db.pool('scheduler');
    const ctx = { ...context('alerts'), caps: { step: 10 } };
    const alerts = [];
    for (const cost of [5, 3, 2, 0]) {
      const result = await reserve(pool, ctx, cost, 'model');
      if (!('reservation_id' in result)) throw new Error('reservation refused');
      alerts.push(...await settle(pool, { reservation_id: result.reservation_id, actual_usd: cost, usage: {} }));
    }
    expect(alerts.map(({ threshold }) => threshold)).toEqual([50, 80, 100]);
  });
});

it.skipIf(!hasTestDatabase)('reaps an expired claimed slot without a sandbox after the teardown margin', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('scheduler');
    const ctx = { ...context('long-step'), caps: { step: 1 } };
    const now = new Date('2026-11-20T00:00:00Z');
    const deadline = new Date(now.getTime() + 600_000);
    const slot = await admit(pool, {
      step_run_id: ctx.stepId, execution_id: ctx.runId, project_id: ctx.projectId,
      agent_id: ctx.agentId, ready_at: now.toISOString(), max_cost_usd: 1,
    }, ctx, { org: 1, group: null, project: null, agent: 5, runner: 1 }, deadline, now);
    if (!slot || !('slot_id' in slot)) throw new Error('admission failed');
    const renewed = new Date(now.getTime() + 900_000);
    const heartbeatAt = new Date(now.getTime() + 300_000);
    const claims = { sub: ctx.stepId, runner_id: ctx.runnerId, group_id: ctx.groupId, agent_id: ctx.agentId,
      project_id: ctx.projectId, agent_version: 'version', claim_id: 'claim', generation: 1, exp: renewed.getTime() / 1000 };
    expect(await admissionModule.refreshSlot(pool, slot.slot_id, claims, heartbeatAt)).toBe(true);
    expect(await admissionModule.refreshSlot(pool, slot.slot_id, { ...claims, claim_id: 'other' }, heartbeatAt)).toBe(false);
    expect((await pool.query('SELECT expires_at FROM slots WHERE id = $1', [slot.slot_id])).rows[0]?.expires_at).toEqual(renewed);
    expect(await reapSlots(pool, deadline)).toBe(0);
    expect(await reapSlots(pool, renewed)).toBe(0);
    const afterMargin = new Date(renewed.getTime() + 60_000);
    const sandboxStarted = () => pool.query('UPDATE slots SET sandbox_started_at = now() WHERE id = $1', [slot.slot_id]);
    await sandboxStarted();
    expect(await reapSlots(pool, afterMargin)).toBe(0);
    await pool.query('UPDATE slots SET sandbox_stopped_at = now() WHERE id = $1', [slot.slot_id]);
    expect(await reapSlots(pool, afterMargin)).toBe(1);
    expect(await reapSlots(pool, afterMargin)).toBe(0);
  } finally {
    await db.drop();
  }
});
