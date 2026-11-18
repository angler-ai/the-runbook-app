import { useState, type ReactNode } from 'react';
import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { EmptyState } from '../layout/EmptyState';
import { Button } from '../primitives/button';
import { StatusPill } from '../status/StatusPill';
import { ActorBadge } from '../status/ActorBadge';
import { MonoRef } from '../status/MonoRef';
import { formatRelativeTime, formatDuration } from '../../lib/format';
import { PlayCircle, Loader2 } from 'lucide-react';

export type ExecutionView = components['schemas']['ExecutionView'] & {
  actor?: components['schemas']['ActorRef'] | null;
  /** Review C5: the runbook's display title, when the caller knows it. */
  runbook_title?: string | null;
};

export interface ExecutionTableProps {
  executions: ExecutionView[];
  onSelectExecution?: (execution: ExecutionView) => void;
  onLoadMore?: () => void;
  hasMore?: boolean;
  isLoadingMore?: boolean;
  emptyState?: ReactNode;
  now?: number;
  className?: string;
  /** A user id as the person's name (the directory); the id otherwise. */
  nameOf?: (userId: string) => string;
}

type ActorRef = components['schemas']['ActorRef'];

/** The label of a run a trigger started (`Schedule nightly`). */
export function triggerLabel(createdBy: string | null | undefined): string | undefined {
  const match = /^(schedule|webhook):(.+)$/.exec(createdBy ?? '');
  if (!match) return undefined;
  return `${match[1] === 'schedule' ? 'Schedule' : 'Webhook'} ${match[2]}`;
}

/** A run's `created_by` principal as an actor for its badge. */
export function startedBy(
  createdBy: string | null | undefined,
  nameOf?: (userId: string) => string,
): ActorRef | null {
  if (!createdBy) return null;
  const [kind, rest] = createdBy.includes(':')
    ? [createdBy.slice(0, createdBy.indexOf(':')), createdBy.slice(createdBy.indexOf(':') + 1)]
    : ['', createdBy];
  if (kind === 'human' || kind === 'user' || (!kind && rest.startsWith('usr_'))) {
    return { kind: 'human', user_id: rest };
  }
  if (kind === 'agent') {
    const sponsor = nameOf?.(rest) ?? rest;
    return { kind: 'agent', name: `Agent for ${sponsor}`, principal_id: rest, session_id: 'default' };
  }
  if (kind === 'service') {
    return { kind: 'agent', name: 'Service token', principal_id: rest, session_id: 'default' };
  }
  // LANG9b: a run its runbook's schedule or webhook trigger started.
  if (kind === 'schedule' || kind === 'webhook') return { kind: 'system' };
  if (createdBy === 'system' || rest.startsWith('sys_')) return { kind: 'system' };
  return { kind: 'agent', name: createdBy, principal_id: createdBy, session_id: 'default' };
}

export function ExecutionTable({
  executions,
  onSelectExecution,
  onLoadMore,
  hasMore = false,
  isLoadingMore = false,
  emptyState,
  now: propNow,
  className,
  nameOf,
}: ExecutionTableProps) {
  const [mountedNow] = useState(() => Date.now());
  const now = propNow ?? mountedNow;

  if (executions.length === 0) {
    if (emptyState) return <>{emptyState}</>;
    return (
      <EmptyState
        icon={PlayCircle}
        title="No executions found"
        description="No execution runs match your criteria."
        className={className}
      />
    );
  }

  return (
    <div data-slot="execution-table-container" className={cn('flex flex-col gap-4', className)}>
      <div className="overflow-x-auto border border-border rounded-lg bg-surface">
        <table className="w-full text-left text-xs border-collapse">
          <thead>
            <tr className="border-b border-border bg-surface-raised text-fg-muted font-medium">
              <th scope="col" className="py-2.5 px-4 font-semibold">Run</th>
              <th scope="col" className="py-2.5 px-4 font-semibold">Status</th>
              <th scope="col" className="py-2.5 px-4 font-semibold">Runbook</th>
              <th scope="col" className="py-2.5 px-4 font-semibold">Started</th>
              <th scope="col" className="py-2.5 px-4 font-semibold">Duration</th>
              <th scope="col" className="py-2.5 px-4 font-semibold text-right">Initiator</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {executions.map((exe) => {
              const isClickable = !!onSelectExecution;
              const runbookRef = exe.runbook_title || exe.runbook_slug || exe.runbook_id || 'runbook';
              // Only a known version: the runs list does not carry one, and an
              // unknown version is not a draft.
              const versionRef = exe.runbook_version != null ? `v${exe.runbook_version}` : null;

              // Duration computation
              let durationStr = '—';
              if (exe.started_at_ms) {
                if (exe.completed_at_ms) {
                  durationStr = formatDuration(exe.completed_at_ms - exe.started_at_ms);
                } else if (exe.status === 'running' || exe.status === 'waiting' || exe.status === 'paused') {
                  durationStr = formatDuration(Math.max(0, now - exe.started_at_ms));
                }
              }

              // Who started it: the run's actor, or its `created_by` principal
              // (`human:<user>`, `agent:<sponsor>`, `service:<token>`, `system`).
              const actorRef = exe.actor ?? startedBy(exe.created_by, nameOf);

              return (
                <tr
                  key={exe.id}
                  data-slot="execution-row"
                  onClick={() => onSelectExecution?.(exe)}
                  className={cn(
                    'group transition-colors',
                    isClickable && 'cursor-pointer hover:bg-surface-raised',
                  )}
                >
                  {/* Title and ID */}
                  <td className="py-3 px-4">
                    <div className="flex flex-col gap-0.5">
                      {isClickable ? (
                        <button
                          type="button"
                          onClick={(e) => {
                            e.stopPropagation();
                            onSelectExecution(exe);
                          }}
                          className="text-left font-semibold text-fg group-hover:text-accent hover:underline focus-visible:outline-2 focus-visible:outline-accent rounded"
                        >
                          {exe.title}
                        </button>
                      ) : (
                        <span className="font-semibold text-fg">{exe.title}</span>
                      )}
                      <MonoRef>{exe.id}</MonoRef>
                    </div>
                  </td>

                  {/* Status */}
                  <td className="py-3 px-4">
                    <StatusPill status={exe.status} />
                  </td>

                  {/* The runbook and its published version (review C5). */}
                  <td className="py-3 px-4">
                    <span className="inline-flex items-center gap-1.5">
                      <span className="text-fg">{runbookRef}</span>
                      {versionRef ? (
                        <span className="font-mono text-2xs text-fg-muted px-1.5 py-0.5 rounded bg-surface-raised border border-border">
                          {versionRef}
                        </span>
                      ) : null}
                    </span>
                  </td>

                  {/* Started relative */}
                  <td className="py-3 px-4 text-fg-muted">
                    {exe.started_at_ms ? formatRelativeTime(exe.started_at_ms, now) : '—'}
                  </td>

                  {/* Duration */}
                  <td className="py-3 px-4 font-mono text-fg-muted">
                    {durationStr}
                  </td>

                  {/* Initiator / Actor */}
                  <td className="py-3 px-4 text-right">
                    {actorRef ? (
                      <div className="inline-flex justify-end">
                        <ActorBadge
                          actor={actorRef}
                          name={
                            actorRef.kind === 'human'
                              ? nameOf?.(actorRef.user_id)
                              : triggerLabel(exe.created_by)
                          }
                        />
                      </div>
                    ) : (
                      <span className="text-fg-muted">—</span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      {/* Cursor paging */}
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
