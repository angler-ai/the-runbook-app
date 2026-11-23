import type pg from 'pg';
import type { EnvironmentBudgets } from '@runbook/runner-shared/contracts/environments';
import type { BudgetContext } from './buckets.ts';

export interface AgentLimits {
  max_cost_usd?: number | null;
  concurrency?: number | null;
  cost_per_day_usd?: number | null;
  cost_per_month_usd?: number | null;
  max_run_cost_usd?: number | null;
  alert_threshold_percent?: number | null;
}

export interface BudgetPolicy {
  org_cost_per_day_usd: string | null;
  org_cost_per_month_usd: string | null;
  cost_per_day_usd: string | null;
  cost_per_month_usd: string | null;
  run_max_cost_usd: string | null;
  project_limits: Record<string, AgentLimits>;
  org_alert_threshold?: number | null;
  group_alert_threshold?: number | null;
}

export function configuredCaps(policy: BudgetPolicy, projectId: string, agentLimits: AgentLimits | undefined,
  stepCap: number, environment?: EnvironmentBudgets): BudgetContext['caps'] {
  const caps: BudgetContext['caps'] = { step: Math.min(stepCap, agentLimits?.max_cost_usd ?? Infinity, environment?.max_step_cost_usd ?? Infinity) };
  const project = policy.project_limits[projectId];
  const run = Math.min(Number(policy.run_max_cost_usd ?? Infinity), agentLimits?.max_run_cost_usd ?? Infinity,
    environment?.max_run_cost_usd ?? Infinity);
  const configured = [
    ['org:day', policy.org_cost_per_day_usd], ['org:month', policy.org_cost_per_month_usd],
    ['group:day', policy.cost_per_day_usd], ['group:month', policy.cost_per_month_usd],
    ['environment:day', environment?.cost_per_day_usd], ['environment:month', environment?.cost_per_month_usd],
    ['project:day', project?.cost_per_day_usd], ['project:month', project?.cost_per_month_usd],
    ['agent:day', agentLimits?.cost_per_day_usd], ['agent:month', agentLimits?.cost_per_month_usd],
    ['run', Number.isFinite(run) ? run : null],
  ] as const;
  for (const [key, value] of configured) {
    if (value !== null && value !== undefined) caps[key] = Number(value);
  }
  return caps;
}

export function configuredAlerts(policy: BudgetPolicy, projectId: string, agent?: AgentLimits, environment?: EnvironmentBudgets) {
  return { org: policy.org_alert_threshold, group: policy.group_alert_threshold,
    project: policy.project_limits[projectId]?.alert_threshold_percent, agent: agent?.alert_threshold_percent,
    environment: environment?.alert_threshold_percent, step: agent?.alert_threshold_percent, run: agent?.alert_threshold_percent };
}

export async function lockBudgetSettings(client: pg.PoolClient) {
  await client.query('SELECT pg_advisory_xact_lock(7419004)');
}
