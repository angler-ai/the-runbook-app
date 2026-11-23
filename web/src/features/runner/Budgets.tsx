import { useState } from 'react';
import { useExecutionDocument } from '../../api/hooks';
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

function stepLink(execution: string, step: string) {
  return `/x/${encodeURIComponent(execution)}?view=steps&step=${encodeURIComponent(step)}`;
}

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
      return budget.execution_id
        ? stepLink(budget.execution_id, budget.id)
        : '/runner/policy#policy-defaults';
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
    return budget.name === budget.id ? `${scopeNames[budget.scope]} ${budget.id}` : budget.name;
  return budget.name;
}

const periodNames = {
  day: 'Daily',
  month: 'Monthly',
  concurrency: 'Concurrent',
  run: 'Per run',
  step: 'Per step',
};

function BudgetTrend({ budget, name }: { budget: Budget; name: string }) {
  const days = budget.daily_spend;
  const first = days[0]?.spend_usd;
  const last = days.at(-1)?.spend_usd;
  if (first == null || last == null || budget.period === 'concurrency') return '—';
  const peak = Math.max(...days.map((day) => day.spend_usd));
  const summary =
    first === last
      ? `Daily spend started and ended at ${dollars(first)}. Peak ${dollars(peak)}.`
      : `Daily spend ${first < last ? 'rose' : 'fell'} from ${dollars(first)} to ${dollars(last)}. Peak ${dollars(peak)}.`;
  return (
    <div className="grid gap-1">
      <Sparkline series={days.map((day) => day.spend_usd)} label={`${name} daily spend`} />
      <details className="text-xs">
        <summary className="cursor-pointer">{summary}</summary>
        <ul className="mt-2 max-h-44 overflow-auto">
          {days.map((day) => (
            <li key={day.date}>
              {day.date} · {dollars(day.spend_usd)}
            </li>
          ))}
        </ul>
      </details>
    </div>
  );
}

function NamedBlockedStep({ step }: { step: Budget['blocked'][number] }) {
  const report = useExecutionDocument(step.execution_id);
  return (
    <BlockedStep
      step={step}
      title={
        report.data?.sections?.find((section) => section.step_run_id === step.step_run_id)?.title
      }
      run={report.data?.header?.title}
    />
  );
}

function BlockedStep({
  step,
  title,
  run,
}: {
  step: Budget['blocked'][number];
  title?: string;
  run?: string;
}) {
  return (
    <li className="grid gap-0.5">
      {step.execution_id ? (
        <>
          <a
            className={textLink + ' break-words'}
            href={stepLink(step.execution_id, step.step_run_id)}
          >
            {step.title ?? title ?? `Step ${step.step_run_id}`}
          </a>
          <a
            className={textLink + ' break-words'}
            href={`/x/${encodeURIComponent(step.execution_id)}`}
          >
            {run ?? `Run ${step.execution_id}`}
          </a>
        </>
      ) : (
        <span className="break-words">{step.title ?? title ?? `Step ${step.step_run_id}`}</span>
      )}
      <span>{step.reason}</span>
    </li>
  );
}

function BlockedBudget({ budget }: { budget: Budget }) {
  const [expanded, setExpanded] = useState(false);
  const count = budget.blocked_count ?? budget.blocked.length;
  if (!count) return <span className="text-fg-muted">None</span>;
  return (
    <details className="min-w-32" onToggle={(event) => setExpanded(event.currentTarget.open)}>
      <summary className="cursor-pointer">
        <StatusPill status="waiting" label={`${count} blocked`} />
      </summary>
      {count > budget.blocked.length && (
        <p className="mt-2 text-xs text-fg-muted">
          {budget.blocked.length} of {count} blocked steps
        </p>
      )}
      <ul className="mt-2 grid max-h-64 gap-2 overflow-auto text-xs">
        {budget.blocked.map((step) =>
          expanded && step.execution_id ? (
            <NamedBlockedStep key={step.step_run_id} step={step} />
          ) : (
            <BlockedStep key={step.step_run_id} step={step} />
          ),
        )}
      </ul>
    </details>
  );
}

export function BudgetTable({
  items,
  projects,
  timeZone,
  filtered = false,
}: {
  items: Budget[];
  projects: Project[];
  timeZone: string;
  filtered?: boolean;
}) {
  if (!items.length)
    return (
      <EmptyState
        icon={Gauge}
        title={filtered ? 'No matching budgets' : 'No budgets configured'}
        description={
          filtered
            ? 'Try a different scope or include unblocked budgets.'
            : 'Set organization limits in Policy or add a budget to an environment.'
        }
        action={
          !filtered && (
            <a className={textLink} href="/runner/policy">
              Open policy
            </a>
          )
        }
      />
    );
  return (
    <div className="grid gap-4">
      <p className="text-sm text-fg-muted">Budget periods use {timeZone}</p>
      <Rows
        label="Runner budgets"
        stackOnMobile
        columns={['Budget', 'Period', 'Used / cap', 'Last 30 days', 'Alert at', 'Blocked now']}
        rows={items.map((budget) => ({
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
            <BudgetTrend budget={budget} name={budgetName(budget, projects)} />,
            budget.alert_threshold_percent == null ? '—' : `${budget.alert_threshold_percent}%`,
            <BlockedBudget budget={budget} />,
          ],
        }))}
      />
    </div>
  );
}

export function Budgets({ status, projects }: { status: RunnerStatus; projects: Project[] }) {
  const [scope, setScope] = useState('all');
  const [blocked, setBlocked] = useState(false);
  const query = new URLSearchParams();
  if (scope !== 'all') query.set('scope', scope);
  if (blocked) query.set('blocked', '1');
  const search = query.toString();
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-end gap-3">
        <Field label="Scope">
          <select
            className={selectClasses + ' min-w-36'}
            value={scope}
            onChange={(event) => setScope(event.target.value)}
          >
            <option value="all">All budgets</option>
            {Object.entries(scopeNames).map(([value, name]) => (
              <option key={value} value={value}>
                {name}
              </option>
            ))}
          </select>
        </Field>
        <label className="flex min-h-10 items-center gap-2 text-sm">
          <input
            type="checkbox"
            className="accent-accent"
            checked={blocked}
            onChange={(event) => setBlocked(event.target.checked)}
          />
          Blocked now
        </label>
      </div>
      <BudgetResults key={search} status={status} projects={projects} search={search} />
    </div>
  );
}

function BudgetResults({
  status,
  projects,
  search,
}: {
  status: RunnerStatus;
  projects: Project[];
  search: string;
}) {
  const path = search ? '/budgets?' + search : '/budgets';
  const query = useRunnerQuery<BudgetOverview>(status, path);
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
        `${path}${search ? '&' : '?'}cursor=${encodeURIComponent(cursor)}`,
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
      <BudgetTable
        items={items}
        projects={projects}
        timeZone={query.data.time_zone}
        filtered={Boolean(search)}
      />
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
