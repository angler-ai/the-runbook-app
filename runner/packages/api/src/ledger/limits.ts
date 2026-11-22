import type { BudgetContext } from './buckets.ts';

export interface AgentLimits {
  concurrency?: number;
  cost_per_day_usd?: number;
}

export interface BudgetPolicy {
  org_cost_per_day_usd: string;
  org_cost_per_month_usd: string | null;
  cost_per_day_usd: string | null;
  cost_per_month_usd: string | null;
  run_max_cost_usd: string | null;
  project_limits: Record<string, AgentLimits>;
}

export function configuredCaps(policy: BudgetPolicy, projectId: string, agentLimits: AgentLimits | undefined,
  stepCap: number): BudgetContext['caps'] {
  const caps: BudgetContext['caps'] = { step: stepCap, 'org:day': Number(policy.org_cost_per_day_usd) };
  const configured = [
    ['org:month', policy.org_cost_per_month_usd], ['group:day', policy.cost_per_day_usd],
    ['group:month', policy.cost_per_month_usd], ['run', policy.run_max_cost_usd],
    ['project:day', policy.project_limits[projectId]?.cost_per_day_usd], ['agent:day', agentLimits?.cost_per_day_usd],
  ] as const;
  for (const [key, value] of configured) {
    if (value !== null && value !== undefined) caps[key] = Number(value);
  }
  return caps;
}
