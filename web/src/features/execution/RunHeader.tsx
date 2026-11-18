// UI-3: the run page's header: what ran (the runbook and version, linked),
// how it is going (status, progress, time), who started it and how, its
// inputs and fields folded, and the actions this person may take.
import { useEffect, useState, type ReactNode } from 'react';
import { Link } from '@tanstack/react-router';
import { useQuery } from '@tanstack/react-query';
import { ChevronRight } from 'lucide-react';
import type { Schemas } from '../../api/client';
import { cn } from '../../ui/primitives/cn';
import { MonoRef, StatusPill } from '../../ui/status';
import { useOrgContext } from '../context/OrgContext';
import type { DirectoryNames } from '../context/names';
import { runbookQueryOptions } from '../editor/hooks';
import { principalActor } from '../home/model';
import { duration } from './overlay';
import type { LiveStatus } from './live';

type ExecutionView = Schemas['ExecutionView'];

/** Re-render every `ms` (an open run's elapsed time). */
function useNow(ms: number, active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return undefined;
    const timer = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(timer);
  }, [ms, active]);
  return now;
}

/** Who started the run and through what, as words. */
/** Why a run ended, in words; nothing when the status says it all. */
export function stoppedBecause(reason: string | null | undefined): string | null {
  switch (reason) {
    case null:
    case undefined:
    case '':
    case 'cancelled':
      return null;
    case 'limit.step_runs_exceeded':
      return 'it reached this server’s limit on steps per run.';
    case 'timeout':
      return 'it ran out of time.';
    case 'limit.run_size_exceeded':
      return 'its record grew past this server’s size limit.';
    case 'execution.outputs_too_large':
      return 'its outputs were too large to keep.';
    default:
      return reason;
  }
}

export function startedBy(header: ExecutionView, names: DirectoryNames): string | null {
  if (header.parent) return 'Called by a parent run';
  if (!header.created_by) return null;
  const actor = principalActor(header.created_by, names.nameOf);
  switch (actor.kind) {
    case 'person':
      return `Started by ${actor.label}`;
    case 'agent':
      return `Started by agent ${actor.label}`;
    case 'service':
      return 'Started by a service token (API, CLI or webhook)';
    case 'run':
      return 'Started by a parent run';
    default:
      return `Started by ${actor.label}`;
  }
}

function clock(ms: number): string {
  return new Date(ms).toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function value(v: unknown): string {
  return typeof v === 'object' && v !== null ? JSON.stringify(v) : String(v);
}

const LIVE_WORDS: Record<LiveStatus, string> = {
  connecting: 'Connecting…',
  live: 'Live',
  reconnecting: 'Reconnecting…',
  polling: 'Polling for updates',
  closed: 'Updating…',
  expired: 'History expired',
};

export function LiveIndicator({ status, terminal }: { status: LiveStatus; terminal: boolean }) {
  const word = terminal ? 'Completed' : LIVE_WORDS[status];
  return (
    <span
      data-slot="live-indicator"
      data-live={!terminal && status === 'live' ? true : undefined}
      className="inline-flex items-center gap-1.5 text-xs text-fg-muted print:hidden"
    >
      <span
        aria-hidden="true"
        className={cn(
          'size-1.5 rounded-full',
          !terminal && status === 'live'
            ? 'bg-mint motion-safe:animate-pulse'
            : !terminal && (status === 'polling' || status === 'reconnecting')
              ? 'bg-warning'
              : 'bg-border-strong',
        )}
      />
      <span role="status">{word}</span>
    </span>
  );
}

function Progress({ done, total }: { done: number; total: number }) {
  const percent = total > 0 ? Math.round((Math.min(done, total) / total) * 100) : 0;
  return (
    <span className="inline-flex items-center gap-2">
      <span
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={total}
        aria-valuenow={Math.min(done, total)}
        aria-label={`${done} of ${total} steps done`}
        className="relative h-1.5 w-28 overflow-hidden rounded-full bg-hover"
      >
        <span
          className="absolute inset-y-0 left-0 rounded-full bg-accent transition-[width] duration-500"
          style={{ width: `${percent}%` }}
        />
      </span>
      <span className="font-mono text-2xs text-fg-muted">
        {Math.min(done, total)} of {total} steps
      </span>
    </span>
  );
}

export interface RunHeaderProps {
  header: ExecutionView;
  names: DirectoryNames;
  live: LiveStatus;
  terminal: boolean;
  actions: ReactNode;
}

export function RunHeader({ header, names, live, terminal, actions }: RunHeaderProps) {
  const { projects } = useOrgContext();
  const project = projects.find((p) => p.id === header.project_id);
  const runbook = useQuery({
    ...runbookQueryOptions(header.runbook_id ?? ''),
    enabled: Boolean(header.runbook_id),
    staleTime: 60_000,
    retry: false,
  });
  const latest = runbook.data?.latest_version ?? null;
  const now = useNow(15_000, !terminal);
  const start = header.started_at_ms ?? header.created_at_ms ?? null;
  const end = header.completed_at_ms ?? null;
  const who = startedBy(header, names);
  const inputs = Object.entries(header.inputs);
  const fields = Object.entries(header.fields);
  const runbookLabel = header.runbook_slug ?? header.runbook_id ?? 'runbook';
  const version = header.runbook_version != null ? `v${header.runbook_version}` : 'preview version';
  const older = header.runbook_version != null && latest != null && latest > header.runbook_version;

  return (
    <header data-slot="run-header" className="grid min-w-0 grid-cols-[minmax(0,1fr)] gap-3">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-fg-muted">
        {project && header.runbook_id ? (
          <Link
            to="/p/$project/runbooks/$rb"
            params={{ project: project.slug, rb: header.runbook_id }}
            className="font-mono text-fg underline-offset-2 hover:underline"
          >
            {runbookLabel}
          </Link>
        ) : (
          <MonoRef>{runbookLabel}</MonoRef>
        )}
        <span className="font-mono">{version}</span>
        {older ? (
          <span
            data-slot="older-version"
            className="rounded-sm border border-border px-1.5 py-px text-2xs"
          >
            Older version · latest is v{latest}
          </span>
        ) : null}
        <ChevronRight className="size-3" aria-hidden="true" />
        <MonoRef className="text-2xs">{header.id}</MonoRef>
      </div>
      <div className="flex flex-wrap items-start justify-between gap-3">
        <h1 className="min-w-0 text-xl font-semibold tracking-tight text-fg sm:text-2xl">
          {header.title}
        </h1>
        <LiveIndicator status={live} terminal={terminal} />
      </div>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-fg-muted">
        <StatusPill status={header.status} />
        {header.progress && header.progress.total > 0 ? (
          <Progress done={header.progress.done} total={header.progress.total} />
        ) : null}
        {who ? <span>{who}</span> : null}
        {header.status === 'queued' ? (
          <span>
            Waiting for a concurrency slot
            {header.created_at_ms != null ? ` · queued ${clock(header.created_at_ms)}` : ''}
          </span>
        ) : start != null ? (
          <span>
            {clock(start)}
            {' · '}
            <span className="font-mono text-xs">
              {end != null ? duration(end - start) : `${duration(now - start)} so far`}
            </span>
          </span>
        ) : null}
        {stoppedBecause(header.terminal_reason) ? (
          <span data-testid="terminal-reason" className="text-fg">
            Stopped: {stoppedBecause(header.terminal_reason)}
          </span>
        ) : null}
        {header.parent ? (
          <a
            href={`/x/${header.parent.execution_id}`}
            className="text-accent underline-offset-2 hover:underline"
          >
            Parent run (step {header.parent.step})
          </a>
        ) : null}
        {header.rerun_of ? (
          <a
            href={`/x/${header.rerun_of}`}
            className="text-accent underline-offset-2 hover:underline"
          >
            Rerun of an earlier run
          </a>
        ) : null}
      </div>
      {inputs.length + fields.length > 0 ? (
        <details data-slot="run-inputs" className="group text-sm">
          <summary className="flex min-w-0 cursor-pointer select-none items-center gap-1 text-xs font-medium text-fg-muted hover:text-fg">
            <ChevronRight
              className="size-3.5 shrink-0 transition-transform group-open:rotate-90"
              aria-hidden="true"
            />
            <span className="shrink-0 whitespace-nowrap">
              {inputs.length} {inputs.length === 1 ? 'input' : 'inputs'}
              {fields.length > 0
                ? ` and ${fields.length} ${fields.length === 1 ? 'field' : 'fields'}`
                : ''}
            </span>
            <span className="ml-1 min-w-0 flex-1 truncate font-mono font-normal">
              {inputs
                .slice(0, 3)
                .map(([k, v]) => `${k}=${value(v)}`)
                .join('  ')}
            </span>
          </summary>
          <dl className="mt-2 grid max-w-3xl grid-cols-[max-content_1fr] gap-x-4 gap-y-1 rounded-md border border-border bg-surface px-3 py-2">
            {inputs.map(([k, v]) => (
              <div key={`i-${k}`} className="contents">
                <dt className="font-mono text-xs leading-6 text-fg-muted">{k}</dt>
                <dd className="min-w-0 break-words font-mono text-xs leading-6 text-fg">
                  {value(v)}
                </dd>
              </div>
            ))}
            {fields.map(([k, v]) => (
              <div key={`f-${k}`} className="contents">
                <dt className="font-mono text-xs leading-6 text-fg-muted">
                  {k} <span className="font-sans">(field)</span>
                </dt>
                <dd className="min-w-0 break-words font-mono text-xs leading-6 text-fg">
                  {value(v)}
                </dd>
              </div>
            ))}
          </dl>
        </details>
      ) : null}
      <div data-slot="run-actions" className="flex flex-wrap items-center gap-2 print:hidden">
        {actions}
      </div>
    </header>
  );
}
