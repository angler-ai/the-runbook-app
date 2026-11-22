import type pg from 'pg';
import { HttpError } from '@runbook/runner-shared/http';

const columns = { step: 'step_run_id', agent: 'agent_id', project: 'project_id', group: 'group_id', runner: 'runner_id', org: null } as const;
const periodNames = ['day', 'month'] as const;
type Period = (typeof periodNames)[number];

export function usageWindow(level: string, period: string, date: string) {
  if (!Object.hasOwn(columns, level) || !periodNames.includes(period as Period) ||
      !/^\d{4}-\d{2}-\d{2}$/.test(date) || !Number.isFinite(Date.parse(date)) ||
      new Date(date).toISOString().slice(0, 10) !== date) {
    throw new HttpError(400, 'usage.invalid_window', 'invalid usage window');
  }
  return { column: columns[level as keyof typeof columns], period: period as Period,
    start: period === 'day' ? date : `${date.slice(0, 7)}-01` };
}

export async function readUsage(pool: pg.Pool, window: ReturnType<typeof usageWindow>, projectIds: string[] | null) {
  const group = window.column ?? 'NULL::text';
  const result = await pool.query(
    `SELECT ${group} AS resource_id, kind, SUM(cost_usd) AS cost_usd, SUM(input_tokens) AS input_tokens,
       SUM(output_tokens) AS output_tokens, SUM(cached_tokens) AS cached_tokens
     FROM usage_events CROSS JOIN policy
     WHERE created_at >= ($1::date::timestamp AT TIME ZONE time_zone)
       AND created_at < (($1::date + $2::interval) AT TIME ZONE time_zone)
       AND ($3::text[] IS NULL OR project_id = ANY($3))
     GROUP BY 1, 2 ORDER BY 1, 2`,
    [window.start, window.period === 'day' ? '1 day' : '1 month', projectIds],
  );
  return result.rows;
}

interface MetricRow {
  provider: string;
  model: string;
  group: string;
  project: string;
  cost: string;
  input: string;
  output: string;
  cached: string;
}

function escaped(value: string): string {
  return value.replaceAll('\\', '\\\\').replaceAll('"', '\\"').replaceAll('\n', '\\n');
}

export function usageMetrics(rows: readonly MetricRow[], leaseLosses: number): string {
  const mapping = { cost: 'cost_usd', input: 'input_tokens', output: 'output_tokens', cached: 'cached_tokens' } as const;
  const lines = ['# TYPE runbook_runner_lease_losses_total counter', `runbook_runner_lease_losses_total ${leaseLosses}`];
  for (const key of Object.keys(mapping) as (keyof typeof mapping)[]) {
    const metric = `runbook_runner_${mapping[key]}_total`;
    lines.push(`# TYPE ${metric} counter`);
    for (const row of rows) {
      const labels = ['provider', 'model', 'group', 'project'].map((name) => {
        const value = row[name as 'provider' | 'model' | 'group' | 'project'];
        return `${name}="${escaped(value)}"`;
      });
      lines.push(`${metric}{${labels.join(',')}} ${row[key]}`);
    }
  }
  return lines.join('\n') + '\n';
}

export async function loadUsageMetrics(pool: pg.Pool) {
  const [usage, leases, pending] = await Promise.all([
    pool.query<MetricRow>(
      `SELECT COALESCE(provider, '') AS provider, COALESCE(model, '') AS model, group_id AS "group", project_id AS project,
       SUM(cost_usd) AS cost, SUM(input_tokens) AS input, SUM(output_tokens) AS output, SUM(cached_tokens) AS cached
       FROM usage_events GROUP BY 1,2,3,4`,
    ),
    pool.query<{ count: number }>(
      "SELECT count(*)::int AS count FROM slots WHERE release_reason = 'lease_expired'",
    ),
    pool.query<{ count: number }>(
      "SELECT count(*)::int AS count FROM ledger_reservations WHERE status = 'settled' AND usage_pending",
    ),
  ]);
  return usageMetrics(usage.rows, leases.rows[0]?.count ?? 0) +
    `# TYPE runbook_runner_pending_reservations gauge\nrunbook_runner_pending_reservations ${pending.rows[0]?.count ?? 0}\n`;
}
