import type pg from 'pg';
import type { Budget } from '@runbook/runner-shared/contracts/budgets';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { HttpError } from '@runbook/runner-shared/http';

type Identity = [Budget['scope'], string, Budget['period']];
type BudgetRow = Omit<Budget, 'cap' | 'spend' | 'percentage'> & { cap: string; spend: string };

function page(target: string) {
  const query = new URL(target, 'http://runner.internal').searchParams;
  const limit = Number(query.get('limit') ?? 100);
  const cursor = query.get('cursor');
  let after: Identity | undefined;
  if (cursor && /^[a-zA-Z0-9_-]{1,1024}$/.test(cursor)) {
    try {
      const parsed: unknown = JSON.parse(Buffer.from(cursor, 'base64url').toString());
      if (Array.isArray(parsed) && parsed.length === 3 && parsed.every((part) => typeof part === 'string')) {
        after = parsed as Identity;
      }
    } catch {
      // Failed decoding is handled below.
    }
  }
  if (!Number.isInteger(limit) || limit < 1 || limit > 100 || (cursor !== null && !after))
    throw new HttpError(400, 'budgets.invalid_page', 'invalid budget page');
  return { limit, after };
}

export async function budgetsOverview(db: pg.Pool, claims: UserAssertionClaims) {
  const admin = claims.org_role === 'owner' || claims.org_role === 'admin';
  const { limit, after } = page(claims.htu ?? '/runner/v1/budgets');
  const projects = admin ? null : Object.keys(claims.project_roles);
  const { rows } = await db.query<BudgetRow & { time_zone: string }>(`
    WITH settings AS (
      SELECT *, (now() AT TIME ZONE time_zone)::date AS today FROM policy WHERE id=1
    ), active_waits AS NOT MATERIALIZED (
      SELECT w.* FROM budget_waiting w CROSS JOIN settings p
      WHERE ($1::text[] IS NULL OR w.project_id=ANY($1))
        AND (CASE WHEN w.period IN ('day','month')
          THEN date_trunc(w.period, w.updated_at AT TIME ZONE p.time_zone) =
               date_trunc(w.period, now() AT TIME ZONE p.time_zone) ELSE true END)
        AND NOT EXISTS (SELECT 1 FROM slots s WHERE s.step_run_id=w.step_run_id AND s.released_at IS NULL)
    ), sources(scope,id,name,project_id,day_cap,month_cap,concurrency,threshold) AS (
      SELECT 'org', $3::text, 'Organization', NULL::text, org_cost_per_day_usd, org_cost_per_month_usd,
        org_concurrency, alert_threshold_percent FROM settings WHERE $2::boolean
      UNION ALL
      SELECT 'group', group_id, name, NULL, cost_per_day_usd, cost_per_month_usd, concurrency, alert_threshold_percent
        FROM groups WHERE $2
      UNION ALL
      SELECT 'environment', e.id, e.name, NULL, (v.budgets->>'cost_per_day_usd')::numeric,
        (v.budgets->>'cost_per_month_usd')::numeric, (v.budgets->>'concurrency')::int,
        (v.budgets->>'alert_threshold_percent')::int
      FROM environments e JOIN LATERAL (
        SELECT config->'budgets' AS budgets FROM environment_versions WHERE environment_id=e.id ORDER BY version DESC LIMIT 1
      ) v ON true
      WHERE $2 AND e.org_id=$3 AND (e.deleted_at IS NULL OR EXISTS (
        SELECT 1 FROM slots s WHERE s.environment_id=e.id AND s.released_at IS NULL))
      UNION ALL
      SELECT 'project', pr.key, pr.key, pr.key, (pr.value->>'cost_per_day_usd')::numeric,
        (pr.value->>'cost_per_month_usd')::numeric, (pr.value->>'concurrency')::int, (pr.value->>'alert_threshold_percent')::int
      FROM settings p CROSS JOIN LATERAL jsonb_each(p.project_limits) pr WHERE $1 IS NULL OR pr.key=ANY($1)
      UNION ALL
      SELECT 'agent', a.id, a.name, a.project_id, (v.config->'limits'->>'cost_per_day_usd')::numeric,
        (v.config->'limits'->>'cost_per_month_usd')::numeric,
        CASE WHEN v.config->'limits' ? 'concurrency' THEN (v.config->'limits'->>'concurrency')::int ELSE p.agent_concurrency END,
        (v.config->'limits'->>'alert_threshold_percent')::int
      FROM agents a JOIN agent_versions v ON v.id=a.current_version_id CROSS JOIN settings p
      WHERE a.status <> 'archived' AND ($1 IS NULL OR a.project_id=ANY($1))
    ), attempt_keys AS (
      SELECT DISTINCT k.scope,k.id,s.project_id FROM slots s
      CROSS JOIN LATERAL (VALUES ('step',s.step_run_id),('run',s.execution_id)) k(scope,id)
      WHERE k.id IS NOT NULL AND ($1 IS NULL OR s.project_id=ANY($1))
        AND (s.released_at IS NULL OR s.created_at >= now() - interval '31 days')
      UNION
      SELECT w.scope,w.budget_key,w.project_id FROM active_waits w WHERE w.scope IN ('run','step')
    ), budgets AS (
      SELECT s.scope,s.id,s.name,s.project_id,b.period,b.cap,
        CASE WHEN b.period <> 'concurrency' THEN s.threshold END AS alert_threshold_percent
      FROM sources s CROSS JOIN LATERAL (VALUES ('day',s.day_cap),('month',s.month_cap),('concurrency',s.concurrency)) b(period,cap)
      WHERE b.cap IS NOT NULL
      UNION ALL
      SELECT k.scope,k.id,k.id,k.project_id,k.scope,LEAST(b.cap_usd, w.cap),b.alert_threshold_percent
      FROM attempt_keys k
      LEFT JOIN ledger_buckets b ON b.level=k.scope AND b.key=k.id AND b.period=k.scope
      LEFT JOIN LATERAL (
        SELECT min(cap) AS cap FROM active_waits WHERE scope=k.scope AND budget_key=k.id AND project_id=k.project_id
      ) w ON true
      WHERE COALESCE(b.cap_usd,w.cap) IS NOT NULL
    ), selected AS MATERIALIZED (
      SELECT * FROM budgets WHERE $4::text IS NULL OR (scope,id,period) > ($4,$5,$6)
      ORDER BY scope,id,period LIMIT $7
    )
    SELECT s.scope,s.id,s.name,s.project_id,s.period,p.time_zone,
      LEAST(s.cap,current.closed_cap_usd,
        CASE WHEN s.scope IN ('agent','environment') AND current.spent_usd + current.overrun_usd >= current.cap_usd
          THEN current.cap_usd END) AS cap,
      s.alert_threshold_percent,
      CASE WHEN s.period='concurrency' THEN (
        SELECT count(*)::numeric FROM slots a WHERE a.released_at IS NULL AND
          CASE s.scope WHEN 'org' THEN true WHEN 'group' THEN a.group_id=s.id
          WHEN 'environment' THEN a.environment_id=s.id WHEN 'project' THEN a.project_id=s.id
          WHEN 'agent' THEN a.agent_id=s.id ELSE false END
      ) ELSE COALESCE(current.spent_usd + current.overrun_usd,0) END AS spend,
      COALESCE(w.blocked, '[]'::jsonb) AS blocked, history.daily_spend
    FROM selected s CROSS JOIN settings p
    LEFT JOIN ledger_buckets current ON current.level=s.scope AND current.key=s.id AND current.period=s.period
      AND current.period_start IS NOT DISTINCT FROM
        CASE s.period WHEN 'day' THEN p.today WHEN 'month' THEN date_trunc('month',p.today::timestamp)::date END
    CROSS JOIN LATERAL (
      SELECT jsonb_agg(jsonb_build_object('step_run_id',step_run_id,'reason',detail) ORDER BY step_run_id) AS blocked
      FROM (SELECT step_run_id,detail FROM active_waits
        WHERE scope=s.scope AND budget_key=s.id AND period=s.period ORDER BY step_run_id LIMIT 25) w
    ) w
    CROSS JOIN LATERAL (
      SELECT jsonb_agg(jsonb_build_object('date', (p.today-n)::text,'spend_usd', COALESCE(b.spend,0)) ORDER BY n DESC) AS daily_spend
      FROM generate_series(29,0,-1) n
      LEFT JOIN LATERAL (
        SELECT period_start AS day, spent_usd + overrun_usd AS spend FROM ledger_buckets
        WHERE s.scope NOT IN ('run','step') AND level=s.scope AND key=s.id AND period='day'
          AND period_start BETWEEN p.today-29 AND p.today
        UNION ALL
        SELECT (u.created_at AT TIME ZONE p.time_zone)::date, sum(u.cost_usd)
        FROM usage_events u LEFT JOIN ledger_reservations r ON r.id=u.reservation_id
        LEFT JOIN slots a ON a.id=r.slot_id
        WHERE s.scope IN ('run','step') AND u.project_id=s.project_id
          AND (s.scope='step' AND u.step_run_id=s.id OR s.scope='run' AND a.execution_id=s.id)
          AND u.created_at >= ((p.today-29)::timestamp AT TIME ZONE p.time_zone)
        GROUP BY 1
      ) b ON b.day=p.today-n
    ) history
    ORDER BY s.scope,s.id,s.period`, [projects, admin, claims.org_id, ...(after ?? [null, null, null]), limit + 1]);

  const time_zone = rows[0]?.time_zone ?? (await db.query<{ time_zone: string }>('SELECT time_zone FROM policy WHERE id=1')).rows[0]?.time_zone ?? 'UTC';
  const items = rows.slice(0, limit).map(({ time_zone: _zone, cap: value, spend: total, ...row }): Budget => {
    const cap = Number(value);
    const spend = Number(total);
    return { ...row, cap, spend, percentage: cap > 0 ? spend * 100 / cap : null };
  });
  const last = items.at(-1);
  const next_cursor = rows.length > limit && last
    ? Buffer.from(JSON.stringify([last.scope, last.id, last.period])).toString('base64url') : null;
  return { time_zone, items, next_cursor };
}
