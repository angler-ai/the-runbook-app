import { DECISION_CONTRACT } from '@runbook/runner-shared/contracts/core';
import type { SlotOutcome } from '@runbook/runner-shared/contracts/internal';
import type pg from 'pg';
import type { CapabilityVerifier } from '@runbook/runner-shared/capability';
import type { ReadyItem, WaitingReason } from '@runbook/runner-shared/contracts/core';
import type { Logger } from '@runbook/runner-shared/log';
import { HttpError, type Route } from '@runbook/runner-shared/http';
import { configuredCaps, type AgentLimits, type BudgetPolicy } from '../ledger/limits.ts';
import { admit, reapSlots, refreshSlot, releaseSandboxSlot, releaseSlot } from './admission.ts';
import { ReadyQueue } from './queue.ts';
import { schedulerMetrics } from './webhook.ts';

interface Runner {
  runner_id: string;
  group_id: string;
  capacity: number;
}

interface Settings extends BudgetPolicy {
  org_concurrency: number;
  agent_concurrency: number;
  agent_max_cost_usd: string;
  time_zone: string;
  group_concurrency: number | null;
}

export function scheduler(options: { db: pg.Pool; coreUrl: string; installId: string; token: string; orgId: string; log?: Logger; verifier?: Pick<CapabilityVerifier, 'verifyCapability'>; notify?: (group: string) => Promise<void>; wakeAlerts?: () => void }) {
  const { db } = options;
  const queues = new Map<string, ReadyQueue>();
  const pending = new Map<string, Promise<ReadyQueue>>();

  async function core(path: string, body?: unknown, signal = AbortSignal.timeout(10_000)) {
    const response = await fetch(`${options.coreUrl}/v1/runner-installs/${encodeURIComponent(options.installId)}${path}`, {
      method: body ? 'POST' : 'GET',
      headers: { 'content-type': 'application/json', authorization: `Bearer ${options.token}` },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      redirect: 'error', signal,
    });
    if (!response.ok) throw new HttpError(502, 'scheduler.core_unavailable', 'scheduler could not reach core');
    return response;
  }

  async function refresh(group: string, project?: string) {
    const key = JSON.stringify([group, project]);
    const existing = pending.get(key);
    if (existing) return existing;
    const operation = (async () => {
      const items: ReadyItem[] = [];
      const visited = new Set<string>();
      const signal = AbortSignal.timeout(10_000);
      let cursor: string | undefined;
      do {
        const query = new URLSearchParams({ group, limit: '100', decision_contract: String(DECISION_CONTRACT) });
        if (cursor) query.set('cursor', cursor);
        if (project) query.set('project', project);
        const result = await core(`/ready?${query}`, undefined, signal);
        const page = await result.json() as {
          items: Array<Omit<ReadyItem, 'ready_at'> & { ready_at: number | string }>;
          next_cursor?: string | null;
        };
        for (const item of page.items) items.push({ ...item, ready_at: new Date(item.ready_at).toISOString() });
        cursor = page.next_cursor ?? undefined;
        if (cursor) {
          if (typeof cursor !== 'string' || cursor.length > 512 || visited.has(cursor)) {
            throw new HttpError(502, 'scheduler.ready_invalid', 'invalid ready page cursor');
          }
          visited.add(cursor);
        }
      } while (cursor);

      const queue = queues.get(group) ?? new ReadyQueue();
      queue.replace(items, project);
      queues.set(group, queue);
      return queue;
    })();
    pending.set(key, operation);
    try {
      return await operation;
    } finally {
      pending.delete(key);
    }
  }

  const routes: Route[] = [
    { method: 'POST', path: '/internal/admission', callers: ['worker'],
      async handle({ body, runnerId }) {
        const value = body as { free_slots?: number; decision_contract?: unknown } | undefined;
        if (!runnerId) throw new HttpError(403, 'scheduler.runner_required', 'runner authentication required');
        if (!value || !Number.isSafeInteger(value.free_slots) || value.free_slots! < 0) {
          throw new HttpError(400, 'scheduler.invalid', 'invalid admission request');
        }
        const { rows: [runner] } = await db.query<Runner>('SELECT runner_id, group_id, capacity FROM runners WHERE runner_id = $1', [runnerId]);
        if (!runner) throw new HttpError(404, 'scheduler.runner_unavailable', 'runner unavailable');
        const queue = await refresh(runner.group_id);
        const { rows: [policy] } = await db.query<Settings>(
          `SELECT p.*, g.concurrency AS group_concurrency, g.cost_per_day_usd, g.cost_per_month_usd
           FROM policy p CROSS JOIN groups g WHERE g.group_id = $1`, [runner.group_id],
        );
        if (!policy) throw new HttpError(503, 'scheduler.policy_unavailable', 'scheduler policy unavailable');
        const now = new Date();
        await reapSlots(db, now);
        const items = [];
        const waiting: { step_run_id: string; reason: WaitingReason }[] = [];
        for (const item of queue.ordered()) {
          if (items.length >= Math.min(value.free_slots!, runner.capacity)) break;
          if (item.kind === 'decision' && value.decision_contract !== DECISION_CONTRACT) continue;
          try {
            const { rows: [agent] } = await db.query<{ status: string; ceiling: { max_cost_usd: number }; config: { limits?: AgentLimits } }>(
              `SELECT a.status, v.ceiling, v.config FROM agents a JOIN agent_versions v ON v.id = a.current_version_id
               JOIN groups g ON g.group_id = a.group_id AND g.projects ? a.project_id
               WHERE a.id = $1 AND a.project_id = $2 AND a.group_id = $3`, [item.agent_id, item.project_id, runner.group_id],
            );
            if (!agent || agent.status !== 'active') {
              waiting.push({ step_run_id: item.step_run_id, reason: 'agent_paused' });
              continue;
            }
            const result = await admit(db, item, {
              orgId: options.orgId, groupId: runner.group_id, runnerId: runner.runner_id, projectId: item.project_id,
              agentId: item.agent_id, runId: item.execution_id, stepId: item.step_run_id, timeZone: policy.time_zone,
              caps: configuredCaps(policy, item.project_id, agent.config.limits,
                Math.min(item.max_cost_usd ?? Number(policy.agent_max_cost_usd), agent.ceiling.max_cost_usd)),
            }, {
              org: policy.org_concurrency, group: policy.group_concurrency, project: policy.project_limits[item.project_id]?.concurrency ?? null,
              agent: agent.config.limits?.concurrency ?? policy.agent_concurrency, runner: runner.capacity,
            }, new Date(now.getTime() + 600_000), now);
            if (!result) continue;
            if ('reason' in result) waiting.push({ step_run_id: item.step_run_id, reason: result.reason });
            else {
              queue.admitted(item);
              items.push({ step_run_id: item.step_run_id, ...(item.kind === 'decision' && { kind: item.kind }), ...result });
            }
          } catch {
            options.log?.error('scheduler item admission failed', { step_run_id: item.step_run_id, runner_id: runnerId });
          }
        }
        if (waiting.length) await core('/waiting', { items: waiting }).catch(() => undefined);
        return { body: { items } };
      } },
    { method: 'POST', path: '/internal/slots/{slot_id}/release', callers: ['worker'],
      async handle({ runnerId, params, body }) {
        if (!runnerId) throw new HttpError(403, 'scheduler.runner_required', 'runner authentication required');
        const outcome = (body as { outcome?: SlotOutcome } | null)?.outcome;
        if (outcome !== undefined && (!outcome || typeof outcome.claim_id !== 'string' ||
            !Number.isSafeInteger(outcome.generation) || outcome.generation < 0 ||
            !['succeeded', 'failed', 'blocked', 'skipped'].includes(outcome.status) ||
            typeof outcome.idle_stop !== 'boolean' || (outcome.idle_stop && outcome.status !== 'failed'))) {
          throw new HttpError(422, 'scheduler.invalid_outcome', 'invalid step outcome');
        }
        const released = await releaseSlot(db, runnerId, params.slot_id ?? '', outcome);
        if (released) options.wakeAlerts?.();
        return { status: released ? 204 : 404 };
      } },
    { method: 'POST', path: '/internal/slots/sandbox/{sandbox_id}/release', callers: ['sandbox-ctl'],
      async handle({ params }) {
        const sandbox = params.sandbox_id ?? '';
        if (!/^rb-[a-f0-9]{64}$/.test(sandbox)) throw new HttpError(400, 'scheduler.invalid', 'invalid sandbox identity');
        await releaseSandboxSlot(db, sandbox);
        options.wakeAlerts?.();
        return { status: 204 };
      } },
    { method: 'POST', path: '/internal/slots/{slot_id}/heartbeat', callers: ['worker'],
      async handle({ runnerId, params, body }) {
        const capability = (body as { capability?: unknown } | undefined)?.capability;
        if (!runnerId || typeof capability !== 'string' || !options.verifier) {
          throw new HttpError(403, 'scheduler.heartbeat_denied', 'slot heartbeat unavailable');
        }
        const claims = await options.verifier.verifyCapability(capability);
        if (claims.runner_id !== runnerId) throw new HttpError(403, 'scheduler.heartbeat_denied', 'slot heartbeat unavailable');
        const refreshed = await refreshSlot(db, params.slot_id ?? '', claims);
        return { status: refreshed ? 204 : 404 };
      } },
  ];

  async function wake(group: string, project?: string) {
    const queue = await refresh(group, project);
    // The end of a webhook receipt transaction must not depend on a
    // worker admission call that needs its pool connection.
    if (queue.size) void options.notify?.(group).catch(() => options.log?.warn('worker notification failed', { group_id: group }));
    return queue;
  }

  return {
    routes,
    wake,
    async sweep() {
      if (await reapSlots(db)) options.wakeAlerts?.();
      await db.query("DELETE FROM work_event_receipts WHERE received_at < now() - interval '7 days'");
      const groups = await db.query<{ group_id: string }>('SELECT group_id FROM groups');
      await Promise.allSettled(groups.rows.map(({ group_id }) => wake(group_id)));
    },
    async metrics() {
      const running = await db.query<{ group: string; runner: string; count: number }>(
        'SELECT group_id AS "group", runner_id AS runner, count(*)::int AS count FROM slots WHERE released_at IS NULL GROUP BY group_id, runner_id',
      );
      return schedulerMetrics([...queues].map(([group, queue]) => ({ group, depth: queue.size })), running.rows);
    },
  };
}
