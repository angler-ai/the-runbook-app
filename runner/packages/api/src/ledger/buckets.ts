import { createHash } from 'node:crypto';
import type pg from 'pg';
import { HttpError } from '@runbook/runner-shared/http';

export type Level = 'environment' | 'step' | 'run' | 'agent' | 'project' | 'group' | 'org';
export type Period = 'step' | 'run' | 'day' | 'month';
type Cap = 'step' | 'run' | `${Exclude<Level, 'step' | 'run'>}:${'day' | 'month'}`;

export interface BudgetContext {
  stepId: string;
  runId: string;
  agentId: string;
  projectId: string;
  environmentId?: string;
  names?: Partial<Record<Level, string>>;
  alerts?: Partial<Record<Level, number | null | undefined>>;
  groupId: string;
  runnerId: string;
  orgId: string;
  timeZone: string;
  pinnedCaps?: Partial<Record<Cap, number>>;
  caps: Partial<Record<Cap, number>>;
}

export function budgetBuckets(context: BudgetContext, now: Date) {
  const parts = new Intl.DateTimeFormat('en-CA', {
    timeZone: context.timeZone, year: 'numeric', month: '2-digit', day: '2-digit',
  }).formatToParts(now);
  const value = (name: string) => parts.find(({ type }) => type === name)?.value;
  const month = `${value('year')}-${value('month')}`;
  const definitions: [Level, string, Period, Cap][] = [
    ['step', context.stepId, 'step', 'step'], ['run', context.runId, 'run', 'run'],
    ['agent', context.agentId, 'day', 'agent:day'], ['agent', context.agentId, 'month', 'agent:month'],
    ['project', context.projectId, 'day', 'project:day'], ['project', context.projectId, 'month', 'project:month'],
    ...(context.environmentId ? [
      ['environment', context.environmentId, 'day', 'environment:day'],
      ['environment', context.environmentId, 'month', 'environment:month'],
    ] as [Level, string, Period, Cap][] : []),
    ['group', context.groupId, 'day', 'group:day'], ['group', context.groupId, 'month', 'group:month'],
    ['org', context.orgId, 'day', 'org:day'], ['org', context.orgId, 'month', 'org:month'],
  ];
  return definitions.map(([level, key, period, cap]) => {
    const start = period === 'day' ? `${month}-${value('day')}` : period === 'month' ? `${month}-01` : null;
    const id = createHash('sha256').update(JSON.stringify([level, key, period, start])).digest('hex');
    return { id, level, key, period, start, cap: context.caps[cap] ?? null, pinned: context.pinnedCaps?.[cap], threshold: context.alerts?.[level] ?? null };
  });
}

export interface BucketRow {
  id: string;
  level: Level;
  key: string;
  period: Period;
  reserved_usd: string;
  spent_usd: string;
  cap_usd: string | null;
  closed_cap_usd?: string | null;
  overrun_usd?: string;
  alerts_sent: number[];
  alert_threshold_percent?: number | null;
  configured_alert_sent?: boolean;
}

export async function lockBuckets(client: pg.PoolClient, ids: string[]): Promise<BucketRow[]> {
  return (await client.query<BucketRow>(
    'SELECT * FROM ledger_buckets WHERE id = ANY($1::text[]) ORDER BY id FOR UPDATE', [ids],
  )).rows;
}

export function micros(value: number | string, direction: 'ceil' | 'floor' | 'nearest' = 'nearest'): number {
  const amount = Number(value);
  const scaled = amount * 1_000_000;
  if (!Number.isFinite(scaled) || scaled < 0 || scaled > 99_999_999_999_999) {
    throw new HttpError(400, 'ledger.invalid_amount', 'invalid ledger amount');
  }
  // Eliminate binary rounding noise around exact microdollar values.
  const nearest = Math.round(scaled);
  if (Math.abs(scaled - nearest) <= Math.max(1, scaled) * Number.EPSILON * 2) return nearest;
  return direction === 'nearest' ? nearest : Math[direction](scaled);
}

export function dollars(amount: number): string {
  return (amount / 1_000_000).toFixed(6);
}
