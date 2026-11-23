import { useState, type ReactNode } from 'react';
import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { EmptyState } from '../layout/EmptyState';
import { Button } from '../primitives/button';
import { Inbox, CheckCircle2, ShieldAlert, ChevronRight, Loader2, Bot } from 'lucide-react';
import { formatRelativeTime } from '../../lib/format';

export type InboxTaskView = components['schemas']['InboxTaskView'];

function startedAgo(startedAtMs: number, now: number): string {
  const ms = Math.max(0, now - startedAtMs);
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 1) return 'started just now';
  if (minutes < 60) return `started ${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `started ${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `started ${days}d ago`;
}

function pathAdds(path: string, title: string): boolean {
  const leaf = path.split('.').pop()?.split('[')[0] ?? path;
  const flat = title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '_')
    .replace(/^_+|_+$/g, '');
  return leaf.length > 0 && flat !== leaf && !flat.startsWith(leaf);
}

export interface InboxRunInfo {
  summary?: string;
  startedAtMs?: number;
  assigned?: string;
  /** R1-D2: the runner agent that may decide this approval, and its state. */
  agentDecides?: {
    name: string;
    readyAtMs?: number;
    reviewing: boolean;
    abstained: boolean;
  };
}

/**
 * R1-D2: an approval a runner agent may decide, in a few words: the agent
 * is reviewing, abstained, or may decide (now, or after `agent.after`).
 */
export function inboxAgentText(
  agent: NonNullable<InboxRunInfo['agentDecides']>,
  now: number,
): string {
  if (agent.reviewing) return `Agent ${agent.name} is reviewing`;
  if (agent.abstained) return `Agent ${agent.name} abstained`;
  if (agent.readyAtMs != null && agent.readyAtMs > now) {
    return `Agent ${agent.name} may decide ${formatRelativeTime(agent.readyAtMs, now)}`;
  }
  return `Agent ${agent.name} may decide now`;
}

export interface InboxListProps {
  items: InboxTaskView[];
  /** Review A: what tells runs of one runbook apart, by execution id. */
  runInfo?: ReadonlyMap<string, InboxRunInfo>;
  /** Clock for relative start times; defaults to mount time. */
  now?: number;
  onSelectTask?: (task: InboxTaskView) => void;
  onLoadMore?: () => void;
  hasMore?: boolean;
  isLoadingMore?: boolean;
  emptyState?: ReactNode;
  className?: string;
}

export function InboxList({
  items,
  runInfo,
  now,
  onSelectTask,
  onLoadMore,
  hasMore = false,
  isLoadingMore = false,
  emptyState,
  className,
}: InboxListProps) {
  const [mountedAt] = useState(() => Date.now());
  const clock = now ?? mountedAt;
  if (items.length === 0) {
    if (emptyState) return <>{emptyState}</>;
    return (
      <EmptyState
        icon={Inbox}
        title="Inbox is clear"
        description="No pending approvals or manual tasks waiting for your action."
        className={className}
      />
    );
  }

  const approvals = items.filter((item) => item.kind === 'approval');
  const tasks = items.filter((item) => item.kind === 'human_task');

  const renderTaskRow = (task: InboxTaskView) => {
    const isClickable = !!onSelectTask;
    const executionHeading = task.execution_title || task.execution_id;
    const info = runInfo?.get(task.execution_id);

    const content = (
      <>
        <div className="flex flex-col gap-1 min-w-0 text-left">
          <div className="flex items-center gap-2">
            <span className="font-semibold text-xs text-fg">{task.title}</span>
            {pathAdds(task.path, task.title) ? (
              <span className="font-mono text-2xs text-fg-muted px-1.5 py-0.2 rounded bg-surface-raised border border-border">
                {task.path}
              </span>
            ) : null}
          </div>

          <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-2xs text-fg-muted">
            <span className="font-medium text-fg">{executionHeading}</span>
            {info?.summary ? <span>{info.summary}</span> : null}
            {info?.startedAtMs && now != null ? (
              <span>{startedAgo(info.startedAtMs, now)}</span>
            ) : null}
            {info?.assigned ? <span>{info.assigned}</span> : null}
          </div>
          {task.kind === 'approval' && info?.agentDecides ? (
            <div
              data-slot="inbox-agent-decides"
              className="flex items-center gap-1 text-2xs text-fg-muted"
            >
              <Bot className="size-3 shrink-0" aria-hidden="true" />
              <span>{inboxAgentText(info.agentDecides, clock)}</span>
            </div>
          ) : null}
        </div>

        <div className="flex items-center gap-2 shrink-0">
          {isClickable && <ChevronRight className="size-4 text-fg-muted" aria-hidden="true" />}
        </div>
      </>
    );

    const baseClass = cn(
      'p-3.5 flex items-center justify-between gap-3 border border-border rounded-lg bg-surface transition-colors w-full',
      isClickable &&
        'cursor-pointer hover:bg-surface-raised hover:border-border-focus focus-visible:bg-surface-raised focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent',
    );

    if (isClickable) {
      return (
        <button
          key={task.step_run_id}
          type="button"
          data-slot="inbox-item-row"
          onClick={() => onSelectTask(task)}
          className={baseClass}
        >
          {content}
        </button>
      );
    }

    return (
      <div key={task.step_run_id} data-slot="inbox-item-row" className={baseClass}>
        {content}
      </div>
    );
  };

  return (
    <div data-slot="inbox-list" className={cn('flex flex-col gap-6', className)}>
      {/* Approvals Group */}
      {approvals.length > 0 && (
        <section aria-labelledby="inbox-approvals-heading" className="flex flex-col gap-2.5">
          <div className="flex items-center gap-2 text-xs font-semibold text-verdict-warn-fg">
            <ShieldAlert className="size-4" aria-hidden="true" />
            <h3
              id="inbox-approvals-heading"
              className="text-xs uppercase tracking-wider font-semibold"
            >
              Approvals ({approvals.length})
            </h3>
          </div>
          <div className="flex flex-col gap-2">{approvals.map(renderTaskRow)}</div>
        </section>
      )}

      {/* Tasks Group */}
      {tasks.length > 0 && (
        <section aria-labelledby="inbox-tasks-heading" className="flex flex-col gap-2.5">
          <div className="flex items-center gap-2 text-xs font-semibold text-accent">
            <CheckCircle2 className="size-4" aria-hidden="true" />
            <h3 id="inbox-tasks-heading" className="text-xs uppercase tracking-wider font-semibold">
              Tasks ({tasks.length})
            </h3>
          </div>
          <div className="flex flex-col gap-2">{tasks.map(renderTaskRow)}</div>
        </section>
      )}

      {/* Cursor paging: Load more button */}
      {hasMore && onLoadMore && (
        <div className="flex justify-center pt-2">
          <Button
            variant="secondary"
            size="default"
            disabled={isLoadingMore}
            onClick={onLoadMore}
            className="min-w-[120px]"
          >
            {isLoadingMore ? (
              <>
                <Loader2 className="size-3.5 animate-spin mr-1.5" aria-hidden="true" />
                Loading...
              </>
            ) : (
              'Load more'
            )}
          </Button>
        </div>
      )}
    </div>
  );
}
