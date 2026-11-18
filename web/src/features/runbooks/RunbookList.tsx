// UI-1: the runbooks list, in Shelf's list-pane manner: dense rows on paper,
// a quiet hover, the title in the text colour and the slug in mono. Each row
// leads with what an ops team asks of a check: did it pass last time, and
// is anything running now. Links render through `renderLink` (router-agnostic,
// so the gallery can show it).
import { ArrowUpRight, GitBranch, MoreHorizontal, PencilLine, Play } from 'lucide-react';
import { useEffect, useRef, type ReactNode } from 'react';
import { formatRelativeTime } from '../../lib/format';
import { Button } from '../../ui/primitives/button';
import { cn } from '../../ui/primitives/cn';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '../../ui/primitives/dropdown-menu';
import { StatusPill } from '../../ui/status/StatusPill';
import { displayTitle, rowHasRunData, runHealth, uniqueTags, type RunbookRow } from './catalog';

/** Why a row shows no run history: the caller cannot read executions. */
export const RUN_DATA_WITHHELD = 'Run history needs executions:read';

export interface RunbookLinks {
  /** The runbook's page (its runs). */
  open(row: RunbookRow): string;
  edit(row: RunbookRow): string;
  versions(row: RunbookRow): string;
  run(executionId: string): string;
}

export type RenderLink = (props: {
  href: string;
  className?: string;
  children: ReactNode;
  'aria-label'?: string;
  'data-row-link'?: boolean;
}) => ReactNode;

export interface RunbookListProps {
  rows: RunbookRow[];
  links: RunbookLinks;
  renderLink: RenderLink;
  onStartRun(row: RunbookRow): void;
  /** Navigate on a menu choice (dropdown items cannot be links). */
  onNavigate(href: string): void;
  canWrite: boolean;
  /** Fixed "now" for tests and the gallery. */
  now?: number;
  className?: string;
}

const HEALTH_RAIL: Record<ReturnType<typeof runHealth>, string> = {
  failing: 'bg-danger',
  attention: 'bg-warning',
  running: 'bg-accent',
  passing: 'bg-mint',
  never: 'bg-border-strong',
  other: 'bg-border-strong',
};

/** ↑/↓ (or j/k), Home and End move between rows' titles; Enter opens. */
function onListKeyDown(list: HTMLElement, event: KeyboardEvent) {
  const keys = ['ArrowDown', 'ArrowUp', 'j', 'k', 'Home', 'End'];
  if (!keys.includes(event.key)) return;
  const target = event.target as HTMLElement;
  if (target.closest('input, textarea, select, [role="menu"]')) return;
  const links = [...list.querySelectorAll<HTMLElement>('[data-row-link]')];
  if (links.length === 0) return;
  const current = links.findIndex((link) => link.closest('li')?.contains(target));
  let next = current;
  if (event.key === 'ArrowDown' || event.key === 'j')
    next = Math.min(links.length - 1, current + 1);
  else if (event.key === 'ArrowUp' || event.key === 'k') next = Math.max(0, current - 1);
  else if (event.key === 'Home') next = 0;
  else next = links.length - 1;
  if (current === -1 && (event.key === 'ArrowUp' || event.key === 'k')) next = 0;
  event.preventDefault();
  links[next]?.focus();
}

export function RunbookList({
  rows,
  links,
  renderLink,
  onStartRun,
  onNavigate,
  canWrite,
  now,
  className,
}: RunbookListProps) {
  const listRef = useRef<HTMLUListElement>(null);
  // A listener rather than onKeyDown: the list itself is not a control, its
  // rows' links are; this only moves focus between them.
  useEffect(() => {
    const list = listRef.current;
    if (!list) return;
    const handler = (event: KeyboardEvent) => onListKeyDown(list, event);
    list.addEventListener('keydown', handler);
    return () => list.removeEventListener('keydown', handler);
  }, []);
  return (
    <div
      className={cn('overflow-hidden rounded-lg border border-border bg-surface', className)}
      data-slot="runbook-list"
    >
      {/* Column heads, desktop only (each row labels its own values for readers). */}
      <div
        aria-hidden="true"
        className="hidden grid-cols-[minmax(0,1fr)_7.5rem_11rem_9.5rem] gap-4 border-b border-border bg-surface-raised px-4 py-2 font-mono text-[10px] font-medium tracking-[0.12em] text-fg-muted uppercase md:grid"
      >
        <span>Runbook</span>
        <span>Version</span>
        <span>Last run</span>
        <span className="text-right">Actions</span>
      </div>
      <ul ref={listRef} aria-label="Runbooks" className="divide-y divide-border">
        {rows.map((row) => (
          <RunbookListRow
            key={row.runbook_id}
            row={row}
            links={links}
            renderLink={renderLink}
            onStartRun={onStartRun}
            onNavigate={onNavigate}
            canWrite={canWrite}
            now={now}
          />
        ))}
      </ul>
    </div>
  );
}

function RunbookListRow({
  row,
  links,
  renderLink,
  onStartRun,
  onNavigate,
  canWrite,
  now,
}: Omit<RunbookListProps, 'rows' | 'className'> & { row: RunbookRow }) {
  const title = displayTitle(row);
  const health = runHealth(row);
  const git = row.origin === 'git';
  const tags = uniqueTags(row.tags);
  const runData = rowHasRunData(row);
  const active = row.active_runs ?? 0;
  const lastRun = row.last_run;

  return (
    <li
      data-slot="runbook-row"
      className="group relative grid gap-x-4 gap-y-2 px-4 py-3.5 transition-colors duration-[120ms] hover:bg-hover focus-within:bg-hover md:grid-cols-[minmax(0,1fr)_7.5rem_11rem_9.5rem] md:items-center"
    >
      {/* Shelf's selection rail, here coloured by the last run's outcome. */}
      <span
        aria-hidden="true"
        className={cn('absolute top-4 bottom-4 left-0 w-[3px] rounded-r-sm', HEALTH_RAIL[health])}
      />

      {/* Identity */}
      <div className="min-w-0">
        <div className="flex min-w-0 flex-wrap items-baseline gap-x-2.5 gap-y-0.5">
          {renderLink({
            href: links.open(row),
            className:
              'min-w-0 truncate rounded-sm text-base font-medium text-fg hover:text-accent focus-visible:outline-offset-1',
            children: title,
            'aria-label': title,
            'data-row-link': true,
          })}
          <span className="truncate font-mono text-2xs text-fg-muted">{row.slug}</span>
          {git ? (
            <span
              className="inline-flex items-center gap-1 rounded border border-border px-1.5 font-mono text-[10px] leading-4 text-fg-muted"
              title="Managed in Git: changed through pull requests"
            >
              <GitBranch className="size-3" aria-hidden="true" />
              git
            </span>
          ) : null}
          {row.archived ? (
            <span className="rounded border border-border px-1.5 font-mono text-[10px] leading-4 text-fg-muted">
              archived
            </span>
          ) : null}
        </div>
        {row.description ? (
          <p className="mt-0.5 truncate text-sm text-fg-muted">
            {row.description}
            {row.description_truncated ? '…' : null}
          </p>
        ) : null}
        {tags.length > 0 ? (
          <ul aria-label="Tags" className="mt-1.5 flex flex-wrap gap-1">
            {tags.map((tag) => (
              <li
                key={tag}
                className="rounded bg-surface-raised px-1.5 font-mono text-[11px] leading-5 text-fg-muted ring-1 ring-border ring-inset"
              >
                {tag}
              </li>
            ))}
          </ul>
        ) : null}
      </div>

      {/* Version */}
      <div className="flex flex-wrap items-center gap-1.5 text-xs">
        <span className="sr-only">Version: </span>
        {row.latest_version != null ? (
          <span className="rounded border border-border px-1.5 font-mono text-2xs leading-5 text-fg">
            v{row.latest_version}
          </span>
        ) : (
          <span className="rounded border border-dashed border-border-strong px-1.5 font-mono text-2xs leading-5 text-fg-muted">
            unpublished
          </span>
        )}
        {row.draft_ahead ? (
          <span
            className="inline-flex items-center gap-1 font-mono text-2xs text-warning-text"
            title="The draft has changes that are not published"
          >
            <span aria-hidden="true" className="size-1.5 rounded-full bg-warning" />
            draft ahead
          </span>
        ) : null}
      </div>

      {/* Last run and activity */}
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-xs">
        <span className="sr-only">Last run: </span>
        {!runData ? (
          <span className="text-fg-muted" title={RUN_DATA_WITHHELD}>
            <span aria-hidden="true">—</span>
            <span className="sr-only">{RUN_DATA_WITHHELD}</span>
          </span>
        ) : lastRun ? (
          <>
            {renderLink({
              href: links.run(lastRun.id),
              className: 'rounded-full focus-visible:outline-offset-1',
              'aria-label': `Last run ${lastRun.status}, ${formatRelativeTime(lastRun.created_at_ms, now)}`,
              children: <StatusPill status={lastRun.status} />,
            })}
            <span className="text-fg-muted tabular-nums">
              {formatRelativeTime(lastRun.created_at_ms, now)}
            </span>
          </>
        ) : (
          <span className="text-fg-muted">Never run</span>
        )}
        {active > 0 ? (
          <span className="inline-flex items-center gap-1.5 font-mono text-2xs text-accent">
            <span aria-hidden="true" className="relative flex size-1.5">
              <span className="absolute inline-flex size-full animate-ping rounded-full bg-accent opacity-60 motion-reduce:hidden" />
              <span className="relative inline-flex size-1.5 rounded-full bg-accent" />
            </span>
            {active >= 100 ? '100+' : active} active
          </span>
        ) : null}
      </div>

      {/* Actions */}
      <div className="flex items-center gap-1 md:justify-end">
        <Button
          variant="secondary"
          size="sm"
          className="h-8 gap-1.5 px-2.5 sm:h-7"
          disabled={row.latest_version == null || row.archived}
          title={row.latest_version == null ? 'Publish a version to start runs' : undefined}
          onClick={() => onStartRun(row)}
          aria-label={`Start a run of ${title}`}
        >
          <Play className="size-3.5" aria-hidden="true" />
          Run
        </Button>
        {renderLink({
          href: links.edit(row),
          className:
            'inline-flex size-8 items-center justify-center rounded-md text-fg-muted hover:bg-surface hover:text-fg sm:size-7',
          'aria-label':
            git || !canWrite ? `View the source of ${title}` : `Edit the draft of ${title}`,
          children: <PencilLine className="size-4" aria-hidden="true" />,
        })}
        <DropdownMenu>
          <DropdownMenuTrigger
            aria-label={`More actions for ${title}`}
            className="inline-flex size-8 items-center justify-center rounded-md text-fg-muted hover:bg-surface hover:text-fg sm:size-7"
          >
            <MoreHorizontal className="size-4" aria-hidden="true" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onSelect={() => onNavigate(links.open(row))}>
              <ArrowUpRight aria-hidden="true" />
              Open runs
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => onNavigate(links.versions(row))}>
              <GitBranch aria-hidden="true" />
              Versions
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => onNavigate(links.edit(row))}>
              <PencilLine aria-hidden="true" />
              {git || !canWrite ? 'View source' : 'Edit draft'}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </li>
  );
}
