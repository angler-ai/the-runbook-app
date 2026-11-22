import type pg from 'pg';
import { HttpError, type Route } from '@runbook/runner-shared/http';

const DAY_MS = 86_400_000;
const sandboxTime = (start: string, end: string) => `
  COALESCE(SUM(GREATEST(0, EXTRACT(EPOCH FROM (
    LEAST(COALESCE(sandbox_stopped_at, ${end}), ${end}) - GREATEST(sandbox_started_at, ${start})
  ))) * 1000) FILTER (WHERE sandbox_started_at IS NOT NULL), 0)::float8 AS total_ms,
  COALESCE(SUM(GREATEST(0, EXTRACT(EPOCH FROM (
    LEAST(COALESCE(sandbox_stopped_at, ${end}), ${end}) - GREATEST(sandbox_ready_at, ${start})
  ))) * 1000) FILTER (WHERE sandbox_ready_at IS NOT NULL), 0)::float8 AS busy_ms`;

interface SandboxTime {
  total_ms: number;
  busy_ms: number;
  idle_stops: number;
}

function time({ total_ms, busy_ms, idle_stops }: SandboxTime) {
  return { busy_ms: Math.round(busy_ms), idle_ms: Math.round(total_ms - busy_ms), idle_stops };
}

export async function agentActivity(pool: pg.Pool, projectId: string, agentId: string, start: Date, end: Date) {
  const week = new Date(end.getTime() - 7 * DAY_MS);
  const [summary, recent] = await Promise.all([
    pool.query<SandboxTime & { completed: number; succeeded: number; running: number; lease_losses: number; p50_ms: number | null }>(
      `SELECT ${sandboxTime('$3', '$4')},
         COUNT(*) FILTER (WHERE reported_at >= $5 AND reported_at <= $4)::int AS completed,
         COUNT(*) FILTER (WHERE reported_at >= $5 AND reported_at <= $4 AND outcome = 'succeeded')::int AS succeeded,
         COUNT(*) FILTER (WHERE reported_at >= $5 AND reported_at <= $4 AND idle_stop)::int AS idle_stops,
         COUNT(*) FILTER (WHERE released_at >= $5 AND released_at <= $4 AND release_reason = 'lease_expired' AND claimed_at IS NOT NULL)::int AS lease_losses,
         COUNT(*) FILTER (WHERE sandbox_ready_at <= $4 AND sandbox_stopped_at IS NULL)::int AS running,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (reported_at - claimed_at)) * 1000)
           FILTER (WHERE reported_at >= $5 AND reported_at <= $4) AS p50_ms
       FROM slots WHERE project_id = $1 AND agent_id = $2 AND (
         (sandbox_started_at <= $4 AND (sandbox_stopped_at IS NULL OR sandbox_stopped_at >= $3))
         OR reported_at >= $5 OR released_at >= $5
       )`, [projectId, agentId, start, end, week],
    ),
    pool.query(
      `SELECT s.step_run_id, s.execution_id, COALESCE(s.outcome,
          CASE WHEN s.released_at IS NULL THEN 'running' ELSE s.release_reason END) AS status,
          (SELECT SUM(u.cost_usd)::float8 FROM usage_events u WHERE u.step_run_id = s.step_run_id
            AND u.runner_id = s.runner_id AND u.created_at >= s.created_at
            AND (s.released_at IS NULL OR u.created_at <= s.released_at)) AS cost_usd
       FROM slots s WHERE s.project_id = $1 AND s.agent_id = $2 AND s.claimed_at >= $3
       ORDER BY s.claimed_at DESC, s.id DESC LIMIT 20`, [projectId, agentId, week],
    ),
  ]);
  const { completed, succeeded, running, lease_losses, p50_ms, ...totals } = summary.rows[0]!;
  return { ...time(totals), completed, succeeded, running, lease_losses, p50_ms,
    success_rate: completed ? succeeded / completed : null, recent: recent.rows };
}

export async function groupActivity(pool: pg.Pool, groups: string[], end = new Date()) {
  const start = new Date(end.getTime() - DAY_MS);
  const { rows } = await pool.query<SandboxTime & { group_id: string }>(
    `SELECT group_id, ${sandboxTime('$2', '$3')},
       COUNT(*) FILTER (WHERE idle_stop AND reported_at >= $2 AND reported_at <= $3)::int AS idle_stops
       FROM slots WHERE group_id = ANY($1::text[]) AND (
         (sandbox_started_at <= $3 AND (sandbox_stopped_at IS NULL OR sandbox_stopped_at >= $2))
         OR reported_at >= $2
       )
       GROUP BY group_id`, [groups, start, end],
  );
  return new Map(rows.map((row) => [row.group_id, time(row)]));
}

export function sandboxMeterRoute(pool: pg.Pool): Route {
  return {
    method: 'POST', path: '/internal/sandboxes/{sandbox_id}/meter', callers: ['sandbox-ctl'],
    async handle({ body, params }) {
      const id = params.sandbox_id ?? '';
      const event = body as { phase?: unknown; at?: unknown } | null;
      const fields = {
        started: ['sandbox_started_at', 'sandbox_stopped_at IS NULL'],
        ready: ['sandbox_ready_at', 'sandbox_started_at <= $2 AND sandbox_stopped_at IS NULL'],
        stopped: ['sandbox_stopped_at', 'sandbox_started_at <= $2 AND COALESCE(sandbox_ready_at, sandbox_started_at) <= $2'],
      } as const;
      if (!/^rb-[a-f0-9]{64}$/.test(id) || typeof event?.phase !== 'string' ||
          !Object.hasOwn(fields, event.phase) || typeof event.at !== 'number' ||
          !Number.isSafeInteger(event.at) || event.at <= 0 || event.at > Date.now() + 60_000) {
        throw new HttpError(422, 'sandbox.invalid_meter', 'invalid sandbox observation');
      }
      const [field, filter] = fields[event.phase as keyof typeof fields];
      await pool.query(
        `UPDATE slots SET ${field} = COALESCE(${field}, $2) WHERE sandbox_id = $1 AND ${filter}`,
        [id, new Date(event.at)],
      );
      return { status: 204 };
    },
  };
}
