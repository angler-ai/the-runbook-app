import { useState } from 'react';
import { Gauge } from 'lucide-react';
import { EmptyState } from '../../ui/layout';
import { StatusPill } from '../../ui/status/StatusPill';
import { Sparkline } from '../../ui/status/Sparkline';
import { dollars } from '../execution/RunnerStep';
import { runnerError, runnerRequest, useRunnerQuery, type RunnerStatus } from './api';
import { Rows } from './presentation';
import { Field } from './presentation';
import { Problem, selectClasses, textLink } from './runnerForms';
import type { Budget, BudgetOverview, Project } from './resources';

function editLink(budget: Budget, projects: Project[]) {
  const project = projects.find(({ id }) => id === budget.project_id);
  switch (budget.scope) {
    case 'environment':
      return `/runner/environments/${budget.id}#environment-budgets`;
    case 'group':
      return `/runner/policy#group-${budget.id}`;
    case 'project':
      return `/runner/policy#project-${budget.id}`;
    case 'agent':
      return project
        ? `/p/${project.slug}/agents/${budget.id}?tab=configuration`
        : '/runner/agents';
    case 'run':
      return `/x/${budget.id}`;
    case 'step':
      return project ? `/p/${project.slug}/runbooks` : '/runner/policy#policy-defaults';
    default:
      return '/runner/policy#policy-budgets';
  }
}

const scopeNames = {
  org: 'Organization',
  group: 'Runner group',
  environment: 'Environment',
  project: 'Project',
  agent: 'Agent',
  run: 'Run',
  step: 'Step',
};

function budgetName(budget: Budget, projects: Project[]) {
  if (budget.scope === 'project')
    return projects.find((project) => project.id === budget.id)?.title ?? 'Project budget';
  if (budget.scope === 'run' || budget.scope === 'step')
    return budget.name === budget.id ? `${scopeNames[budget.scope]} budget` : budget.name;
  return budget.name;
}

const periodNames = {
  day: 'Daily',
  month: 'Monthly',
  concurrency: 'Concurrent',
  run: 'Per run',
  step: 'Per step',
};

export function BudgetTable({
  items,
  projects,
  timeZone,
}: {
  items: Budget[];
  projects: Project[];
  timeZone: string;
}) {
  const [scope, setScope] = useState('all');
  if (!items.length)
    return (
      <EmptyState
        icon={Gauge}
        title="No budgets configured"
        description="Set organization limits in Policy or add a budget to an environment."
        action={
          <a className={textLink} href="/runner/policy">
            Open policy
          </a>
        }
      />
    );
  const visible = items.filter((item) => scope === 'all' || item.scope === scope);
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <Field label="Scope">
          <select
            className={selectClasses + ' min-w-36'}
            value={scope}
            onChange={(event) => setScope(event.target.value)}
          >
            <option value="all">All budgets</option>
            {[...new Set(items.map((item) => item.scope))].map((value) => (
              <option key={value} value={value}>
                {scopeNames[value]}
              </option>
            ))}
          </select>
        </Field>
        <p className="text-sm text-fg-muted">Budget periods use {timeZone}</p>
      </div>
      <Rows
        label="Runner budgets"
        stackOnMobile
        columns={['Budget', 'Period', 'Used / cap', 'Last 30 days', 'Alert at', 'Blocked now']}
        rows={visible.map((budget) => ({
          id: `${budget.scope}:${budget.id}:${budget.period}`,
          cells: [
            <>
              <a className={textLink} href={editLink(budget, projects)}>
                {budgetName(budget, projects)}
              </a>
              <p className="text-xs text-fg-muted ">{scopeNames[budget.scope]}</p>
            </>,
            periodNames[budget.period],
            <div className="min-w-28">
              <p className="font-mono tabular-nums whitespace-nowrap">
                {budget.period === 'concurrency'
                  ? `${budget.spend} / ${budget.cap}`
                  : `${dollars(budget.spend)} / ${dollars(budget.cap)}`}
              </p>
              {budget.percentage !== null && (
                <div className="mt-2 flex items-center gap-2">
                  <progress
                    aria-label={`${budgetName(budget, projects)} budget used`}
                    value={Math.min(100, budget.percentage)}
                    max={100}
                    className={`h-1.5 w-16 appearance-none overflow-hidden rounded-full bg-surface-raised [&::-webkit-progress-bar]:bg-surface-raised ${budget.percentage >= 100 ? '[&::-webkit-progress-value]:bg-danger [&::-moz-progress-bar]:bg-danger' : '[&::-webkit-progress-value]:bg-accent [&::-moz-progress-bar]:bg-accent'}`}
                  />
                  <span className="text-xs text-fg-muted">{Math.round(budget.percentage)}%</span>
                </div>
              )}
            </div>,
            budget.daily_spend.length ? (
              <Sparkline
                series={budget.daily_spend.map((day) => day.spend_usd)}
                label={`${budgetName(budget, projects)} spend over the last 30 days`}
              />
            ) : (
              '—'
            ),
            budget.alert_threshold_percent == null ? '—' : `${budget.alert_threshold_percent}%`,
            budget.blocked.length ? (
              <details className="min-w-32">
                <summary className="cursor-pointer">
                  <StatusPill status="waiting" label={`${budget.blocked.length} blocked`} />
                </summary>
                <ul className="mt-2 grid gap-1 text-xs">
                  {budget.blocked.map((step) => (
                    <li key={step.step_run_id}>{step.reason}</li>
                  ))}
                </ul>
              </details>
            ) : (
              <span className="text-fg-muted">None</span>
            ),
          ],
        }))}
      />
    </div>
  );
}

export function Budgets({ status, projects }: { status: RunnerStatus; projects: Project[] }) {
  const query = useRunnerQuery<BudgetOverview>(status, '/budgets');
  const [more, setMore] = useState<{ items: Budget[]; cursor: string | null } | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [moreError, setMoreError] = useState<unknown>(null);
  if (query.error) return <Problem>{runnerError(query.error)}</Problem>;
  if (!query.data)
    return (
      <p role="status" className="text-sm text-fg-muted">
        Loading budgets…
      </p>
    );
  const items = [...query.data.items, ...(more?.items ?? [])];
  const cursor = more ? more.cursor : (query.data.next_cursor ?? null);
  const install = status.install_id;
  async function loadMore() {
    if (!cursor || !install) return;
    setLoadingMore(true);
    setMoreError(null);
    try {
      const page = await runnerRequest<BudgetOverview>(
        install,
        `/budgets?cursor=${encodeURIComponent(cursor)}`,
        'GET',
      );
      setMore({ items: [...(more?.items ?? []), ...page.items], cursor: page.next_cursor ?? null });
    } catch (error) {
      setMoreError(error);
    } finally {
      setLoadingMore(false);
    }
  }
  return (
    <div className="grid gap-3">
      <BudgetTable items={items} projects={projects} timeZone={query.data.time_zone} />
      {moreError ? <Problem>{runnerError(moreError)}</Problem> : null}
      {cursor ? (
        <div>
          <button
            type="button"
            className={textLink + ' text-sm'}
            disabled={loadingMore}
            onClick={() => void loadMore()}
          >
            {loadingMore ? 'Loading more budgets…' : 'Show more budgets'}
          </button>
        </div>
      ) : null}
    </div>
  );
}
