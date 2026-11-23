import { createHmac, timingSafeEqual } from 'node:crypto';
import type http from 'node:http';
import type pg from 'pg';
import { withTransaction } from '@runbook/runner-shared/db';

function label(value: string): string {
  return value.replaceAll('\\', '\\\\').replaceAll('"', '\\"').replaceAll('\n', '\\n');
}

export function schedulerMetrics(queue: readonly { group: string; depth: number }[],
  running: readonly { group: string; runner: string; count: number }[]): string {
  const lines = [
    '# TYPE runbook_runner_queue_depth gauge',
    ...queue.map(({ group, depth }) => `runbook_runner_queue_depth{group="${label(group)}"} ${depth}`),
    '# TYPE runbook_runner_running gauge',
    ...running.map(({ group, runner, count }) => `runbook_runner_running{group="${label(group)}",runner="${label(runner)}"} ${count}`),
  ];
  return lines.join('\n') + '\n';
}

export function authenticWebhook(body: Buffer, signature: string, secret: string, timestamp: string, now: Date): boolean {
  if (!/^\d{1,12}$/.test(timestamp) || !secret) return false;
  const at = Number(timestamp);
  if (Math.abs(Math.floor(now.getTime() / 1000) - at) > 300) return false;
  const expected = createHmac('sha256', secret).update(`${at}.`).update(body).digest();
  return signature.split(',').some((entry) => {
    const hex = /^v1=([0-9a-f]{64})$/.exec(entry.trim())?.[1];
    return hex !== undefined && timingSafeEqual(expected, Buffer.from(hex, 'hex'));
  });
}

export function coreWebhook(pool: pg.Pool, options: {
  secret?: string; orgId: string; wake(group: string, project?: string): Promise<unknown>; now?: () => Date;
}): http.RequestListener {
  return async (req, res) => {
    const end = (status: number) => { res.writeHead(status, { 'cache-control': 'no-store' }).end(); };
    if (req.method !== 'POST') return end(405);
    const signature = req.headers['runbook-signature'];
    const timestamp = req.headers['runbook-timestamp'];
    if (!options.secret || typeof signature !== 'string' || typeof timestamp !== 'string') return end(401);
    try {
      const chunks: Buffer[] = [];
      let size = 0;
      for await (const chunk of req) {
        const bytes = chunk as Buffer;
        size += bytes.length;
        if (size > 256 * 1024) return end(413);
        chunks.push(bytes);
      }
      const body = Buffer.concat(chunks);
      const now = options.now?.() ?? new Date();
      if (!authenticWebhook(body, signature, options.secret, timestamp, now)) return end(401);

      let event: { id?: unknown; type?: unknown; org?: unknown; project?: unknown; version?: unknown; data?: { ready_steps?: unknown } };
      try {
        event = JSON.parse(body.toString('utf8')) as typeof event;
      } catch {
        return end(400);
      }
      if (!event || event.org !== options.orgId) return end(403);
      if (event.type !== 'agent.work_available') return end(204);
      if (typeof event.id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(event.id) ||
          event.version !== 1 || !Array.isArray(event.data?.ready_steps)) return end(400);
      const groups = [...new Set(event.data.ready_steps.flatMap((step: { group_id?: unknown } | null) =>
        typeof step?.group_id === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(step.group_id) ? [step.group_id] : []))];
      const project = typeof event.project === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(event.project)
        ? event.project : undefined;
      await withTransaction(pool, async (client) => {
        const added = await client.query(
          'INSERT INTO work_event_receipts(id, received_at) VALUES ($1,$2) ON CONFLICT (id) DO NOTHING', [event.id, now],
        );
        if (added.rowCount) await Promise.all(groups.map((group) => options.wake(group, project)));
      });
      return end(204);
    } catch {
      return end(503);
    }
  };
}
