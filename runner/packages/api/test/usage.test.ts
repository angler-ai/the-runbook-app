import { expect, it } from 'vitest';
import { agentActivity, sandboxMeterRoute } from '../src/resources/activity.ts';
import { loadUsageMetrics, usageMetrics, usageWindow } from '../src/resources/usage.ts';
import { readTranscript, transcriptAllowed } from '../src/resources/transcripts.ts';
import { gatewayResources } from '../src/resources/gateway.ts';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import { exportAudit } from '../src/resources/audit.ts';
import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';

it('usage_rolls_up_by_level_and_period', () => {
  expect(usageWindow('project', 'day', '2026-11-20')).toEqual({
    column: 'project_id', period: 'day', start: '2026-11-20',
  });
});
it('transcript_visible_only_to_project_members', () => {
  expect(transcriptAllowed({ project_roles: { p: 'viewer' }, scopes: ['runner:read'] }, 'p')).toBe(true);
  expect(transcriptAllowed({ project_roles: { p: 'viewer' }, scopes: ['runner:read'] }, 'q')).toBe(false);
});
it.skipIf(!hasTestDatabase)('binds transcript references to the verified lease across retries', async () => {
  const db = await createTestDatabase();
  try {
    const pool = db.pool('api');
    const verifyCapability = async (token: string) => ({
      sub: 'step', group_id: 'g', runner_id: 'r', project_id: 'p', agent_id: 'a',
      claim_id: token, generation: token === 'clm_first' ? 1 : 2,
    }) as GrantClaims;
    const route = gatewayResources(pool, { verifyCapability }).find(({ path }) => path === '/internal/transcripts')!;
    for (const lease of ['clm_first', 'clm_second']) {
      await route.handle({ body: { capability: lease, role: 'tool', value: { reference: 'call 1', result: lease } },
        caller: 'gateway', params: {} });
    }
    await db.admin().query(`INSERT INTO transcripts(step_run_id,group_id,runner_id,project_id,agent_id,entry)
      VALUES ('step','g','r','p','a','{"role":"tool","value":{"reference":"call 1"}}')`);
    const user = { project_roles: { p: 'viewer' as const }, scopes: ['runner:read' as const] };
    expect(await readTranscript(pool, user, 'step', 0, 'clm_second')).toEqual([expect.objectContaining({
      claim_id: 'clm_second', generation: 2, entry: { role: 'tool', value: { reference: 'call 1', result: 'clm_second' } },
    })]);
    expect(await readTranscript(pool, user, 'step')).toEqual([
      expect.objectContaining({ claim_id: 'clm_first', generation: 1 }),
      expect.objectContaining({ claim_id: 'clm_second', generation: 2 }),
      expect.objectContaining({ claim_id: null, generation: null }),
    ]);
  } finally {
    await db.drop();
  }
});
it.skipIf(!hasTestDatabase)('audit_export_verifies_chain', async () => {
  const db = await createTestDatabase();
  try {
    expect(await exportAudit(db.pool('api'))).toMatchObject({ chain: { ok: true }, items: [] });
  } finally {
    await db.drop();
  }
});
it('metrics_cover_cost_tokens_and_lease_losses', () => {
  const metrics = usageMetrics([{ provider: 'example', model: 'test', group: 'default', project: 'ops',
    cost: '0.5', input: '10', output: '7', cached: '2' }], 1);
  expect(metrics).toContain('runbook_runner_cost_usd_total{provider="example",model="test",group="default",project="ops"} 0.5');
  expect(metrics).toContain('runbook_runner_input_tokens_total');
  expect(metrics).toContain('runbook_runner_lease_losses_total 1');
});

it.skipIf(!hasTestDatabase)('activity uses measured sandbox time and reported outcomes', async () => {
  const db = await createTestDatabase();
  try {
    await db.admin().query(`INSERT INTO slots(id,step_run_id,group_id,runner_id,project_id,agent_id,execution_id,
      claimed_at,sandbox_started_at,sandbox_ready_at,sandbox_stopped_at,outcome,reported_at,idle_stop)
      VALUES ('one','first','g','r','p','a','x1','2026-11-20T00:00:00Z','2026-11-20T00:00:01Z',
        '2026-11-20T00:00:03Z','2026-11-20T00:00:08Z','succeeded','2026-11-20T00:00:10Z',false),
      ('two','next','g','r','p','a','x2','2026-11-20T00:00:00Z','2026-11-20T00:00:02Z',
        '2026-11-20T00:00:04Z','2026-11-20T00:00:10Z','failed','2026-11-20T00:00:20Z',true),
      ('other','private','g','r','q','a','x3',NULL,NULL,NULL,NULL,NULL,NULL,false)`);
    const data = await agentActivity(db.pool('api'), 'p', 'a', new Date('2026-11-19'), new Date('2026-11-21'));
    expect(data).toMatchObject({ busy_ms: 11_000, idle_ms: 4000, idle_stops: 1, p50_ms: 15_000,
      completed: 2, succeeded: 1, success_rate: 0.5, running: 0, recent: [
        { step_run_id: 'next', execution_id: 'x2', status: 'failed' },
        { step_run_id: 'first', execution_id: 'x1', status: 'succeeded' },
      ] });
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('sandbox lifecycle writes are claim-bound and keep the first timestamps on retry', async () => {
  const db = await createTestDatabase();
  const id = 'rb-' + 'a'.repeat(64);
  try {
    const pool = db.pool('scheduler');
    await db.admin().query(`INSERT INTO slots(id,step_run_id,group_id,runner_id,project_id,agent_id,sandbox_id)
      VALUES ('one','step','g','r','p','a',$1)`, [id]);
    const route = sandboxMeterRoute(pool);
    const start = Date.now() - 10000;
    const meter = (phase: string, at: number) => route.handle({
      caller: 'sandbox-ctl', params: { sandbox_id: id }, body: { phase, at },
    });
    expect(route.callers).toEqual(['sandbox-ctl']);
    await meter('started', start);
    await meter('started', start + 1);
    await meter('ready', start + 250);
    await meter('stopped', start + 1000);
    await meter('stopped', start + 1250);
    const activity = await agentActivity(db.pool('api'), 'p', 'a', new Date(start), new Date());
    expect(activity).toMatchObject({ busy_ms: 750, idle_ms: 250, running: 0, success_rate: null });
  } finally {
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('exports the pending reservation count as a gauge', async () => {
  const db = await createTestDatabase();
  try {
    const api = db.pool('api');
    expect(await loadUsageMetrics(api)).toContain(
      '# TYPE runbook_runner_pending_reservations gauge\nrunbook_runner_pending_reservations 0\n',
    );
    await db.admin().query(`
      INSERT INTO ledger_reservations(id,step_run_id,group_id,runner_id,bucket_ids,kind,amount_usd,status,usage_pending)
      VALUES ('pending','s','g','r','[]','model',1,'settled',true),
        ('measured','s','g','r','[]','model',1,'settled',false)
    `);
    expect(await loadUsageMetrics(api)).toContain('runbook_runner_pending_reservations 1\n');
  } finally {
    await db.drop();
  }
});
