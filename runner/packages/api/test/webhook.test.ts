import http from 'node:http';
import { createHmac } from 'node:crypto';
import { expect, it, vi } from 'vitest';
import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import * as webhook from '../src/scheduler/webhook.ts';
import * as alerts from '../src/ledger/alerts.ts';
import { reserve } from '../src/ledger/reserve.ts';
import { settle } from '../src/ledger/settle.ts';

it.skipIf(!hasTestDatabase)('verifies the core webhook wire format and consumes work events once across replicas', async () => {
  const db = await createTestDatabase();
  const wake = vi.fn(async () => {});
  const secret = 'fixture-signing-key';
  const now = new Date('2026-11-20T00:00:00Z');
  const handler = () => webhook.coreWebhook(db.pool('scheduler'), { secret, orgId: 'org', wake, now: () => now });
  const first = http.createServer(handler());
  const second = http.createServer(handler());
  const listen = async (server: http.Server) => {
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('missing address');
    return `http://127.0.0.1:${address.port}/internal/webhooks/core`;
  };
  try {
    const [a, b] = await Promise.all([listen(first), listen(second)]);
    const body = JSON.stringify({ id: 'evt_test', type: 'agent.work_available', version: 1, org: 'org', project: 'p',
      data: { ready_steps: [{ group_id: 'g', step_run_id: 'step', agent_id: 'agent' }] } });
    const send = (url: string, key = secret, timestamp = Math.floor(now.getTime() / 1000)) => {
      const signature = createHmac('sha256', key).update(`${timestamp}.`).update(body).digest('hex');
      return fetch(url, { method: 'POST', body, headers: {
        'Runbook-Timestamp': String(timestamp), 'Runbook-Signature': `v1=${signature}`,
      } });
    };
    expect((await send(a, 'wrong')).status).toBe(401);
    expect((await send(a, secret, Math.floor(now.getTime() / 1000) - 301)).status).toBe(401);
    expect(wake).not.toHaveBeenCalled();
    expect((await send(a)).status).toBe(204);
    expect((await send(b)).status).toBe(204);
    expect(wake.mock.calls).toEqual([['g', 'p']]);
  } finally {
    await Promise.all([first, second].map((server) => new Promise<void>((resolve) => server.close(() => resolve()))));
    await db.drop();
  }
});

it.skipIf(!hasTestDatabase)('queues a threshold within settlement and retries failed event delivery', async () => {
  const db = await createTestDatabase();
  const pool = db.pool('scheduler');
  try {
    const result = await reserve(pool, { stepId: 's', runId: 'r', agentId: 'a', projectId: 'p', groupId: 'g',
      runnerId: 'runner', orgId: 'org', timeZone: 'UTC', caps: { 'org:day': 10 } }, 5, 'model');
    if (!('reservation_id' in result)) throw new Error('reservation refused');
    await settle(pool, { reservation_id: result.reservation_id, actual_usd: 5, usage: {} });

    const sender = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(new Response(null, { status: 503 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const delivery = { coreUrl: 'https://core.example', installId: 'install', token: 'fixture', fetch: sender };
    const now = new Date(Date.now() + 1000);
    await alerts.deliverBudgetAlerts(pool, delivery, now);
    await alerts.deliverBudgetAlerts(pool, delivery, now);
    expect(sender).toHaveBeenCalledTimes(1);
    await alerts.deliverBudgetAlerts(pool, delivery, new Date(now.getTime() + 60_000));
    expect(sender).toHaveBeenCalledTimes(2);
    expect(sender.mock.calls[1]?.[1]?.body).toBe(sender.mock.calls[0]?.[1]?.body);
    expect(JSON.parse(String(sender.mock.calls[1]?.[1]?.body))).toMatchObject({
      kind: 'runner.budget_threshold', data: { level: 'org', threshold: 50, runner_id: 'runner' },
    });
    expect((await pool.query('SELECT id FROM budget_event_outbox')).rows).toEqual([]);
  } finally {
    await db.drop();
  }
});
