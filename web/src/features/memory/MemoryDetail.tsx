import { useState } from 'react';
import { Archive, Check, History, Lock, Pencil, RotateCcw, Unlock, X } from 'lucide-react';
import { formatRelativeTime } from '../../lib/format';
import { Button } from '../../ui/primitives/button';
import { EditMemoryDialog } from './EditMemoryDialog';
import {
  formatMemoryWriter,
  memoryErrorText,
  useMemoryRevisions,
  usePatchMemory,
  type MemoryEntry,
  type MemoryPatchRequest,
} from './hooks';

export interface MemoryDetailProps {
  entry: MemoryEntry;
  runbookId?: string;
  projectId?: string;
  canMutate: boolean;
  nameOf(userId: string): string;
  onEntryUpdated?(updated: MemoryEntry): void;
  onReload?(): void;
}

export function MemoryDetail({
  entry,
  runbookId,
  projectId,
  canMutate,
  nameOf,
  onEntryUpdated,
  onReload,
}: MemoryDetailProps) {
  const [editOpen, setEditOpen] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const revisionsQuery = useMemoryRevisions(entry.id);
  const patchMutation = usePatchMemory({ runbookId, projectId });

  const handleAction = async (patch: MemoryPatchRequest) => {
    setActionError(null);
    try {
      const updated = await patchMutation.mutateAsync({
        memoryId: entry.id,
        revision: entry.revision,
        patch,
      });
      onEntryUpdated?.(updated);
    } catch (err) {
      const msg = memoryErrorText(err);
      setActionError(msg);
      if (msg === 'This entry changed since you opened it.') {
        onReload?.();
      }
    }
  };

  const writer = formatMemoryWriter(entry.last_writer, entry.source, nameOf);

  const fallbackStatus = {
    label: 'Active',
    className: 'bg-verdict-pass-bg text-verdict-pass-fg border-verdict-pass-border',
  };

  const statusVariant: Record<string, { label: string; className: string }> = {
    active: fallbackStatus,
    proposed: {
      label: 'Proposed',
      className: 'bg-verdict-warn-bg text-verdict-warn-fg border-verdict-warn-border',
    },
    archived: {
      label: 'Archived',
      className: 'bg-surface-raised text-fg-muted border-border',
    },
  };

  const statusInfo = statusVariant[entry.status] ?? fallbackStatus;

  const revisions = (revisionsQuery.data?.pages ?? []).flatMap((p) => p.items);
  const pruned = revisionsQuery.data?.pages[0]?.pruned ?? entry.revisions_pruned ?? 0;

  return (
    <div data-slot="memory-detail" className="flex flex-col gap-6">
      {/* Header and badges */}
      <div className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-2">
            <span
              data-slot="memory-status-badge"
              className={`inline-flex items-center rounded-md border px-2 py-0.5 text-xs font-medium ${statusInfo.className}`}
            >
              {statusInfo.label}
            </span>
            {entry.locked ? (
              <span
                data-slot="memory-locked-badge"
                className="inline-flex items-center gap-1 rounded-md border border-border bg-surface-raised px-2 py-0.5 text-xs font-medium text-fg-muted"
              >
                <Lock className="size-3" aria-hidden="true" />
                Locked
              </span>
            ) : null}
            <span className="font-mono text-2xs text-fg-muted">r{entry.revision}</span>
          </div>

          {/* Action buttons for authorized person */}
          {canMutate ? (
            <div
              className="flex flex-wrap items-center gap-1.5"
              role="toolbar"
              aria-label="Memory actions"
            >
              {entry.status === 'proposed' ? (
                <>
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={patchMutation.isPending}
                    onClick={() => handleAction({ action: 'accept' })}
                  >
                    <Check className="mr-1 size-3.5" aria-hidden="true" />
                    Accept
                  </Button>
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={patchMutation.isPending}
                    onClick={() => handleAction({ action: 'reject' })}
                  >
                    <X className="mr-1 size-3.5" aria-hidden="true" />
                    Reject
                  </Button>
                </>
              ) : null}

              {entry.status === 'active' ? (
                <>
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={patchMutation.isPending}
                    onClick={() => setEditOpen(true)}
                  >
                    <Pencil className="mr-1 size-3.5" aria-hidden="true" />
                    Edit
                  </Button>
                  {entry.locked ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={patchMutation.isPending}
                      onClick={() => handleAction({ action: 'unlock' })}
                    >
                      <Unlock className="mr-1 size-3.5" aria-hidden="true" />
                      Unlock
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={patchMutation.isPending}
                      onClick={() => handleAction({ action: 'lock' })}
                    >
                      <Lock className="mr-1 size-3.5" aria-hidden="true" />
                      Lock
                    </Button>
                  )}
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={patchMutation.isPending}
                    onClick={() => handleAction({ action: 'archive' })}
                  >
                    <Archive className="mr-1 size-3.5" aria-hidden="true" />
                    Archive
                  </Button>
                </>
              ) : null}

              {entry.status === 'archived' ? (
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={patchMutation.isPending}
                  onClick={() => handleAction({ action: 'restore', revision: entry.revision })}
                >
                  <RotateCcw className="mr-1 size-3.5" aria-hidden="true" />
                  Restore
                </Button>
              ) : null}
            </div>
          ) : null}
        </div>

        <h3 className="text-lg font-semibold tracking-tight text-fg">
          {entry.title || entry.body_markdown.split('\n')[0] || entry.id}
        </h3>

        {actionError ? (
          <p
            role="alert"
            className="rounded-md border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger"
          >
            {actionError}
          </p>
        ) : null}

        {/* Metadata row */}
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-xs text-fg-muted">
          <span>
            Writer: <span className="font-medium text-fg">{writer.label}</span>
          </span>
          {entry.updated_at ? (
            <span title={new Date(entry.updated_at).toLocaleString()}>
              Updated {formatRelativeTime(entry.updated_at)}
            </span>
          ) : null}
          {entry.step_id ? (
            <span>
              Step: <span className="font-mono text-fg">{entry.step_id}</span>
            </span>
          ) : null}
        </div>

        {/* Tags */}
        {(entry.tags ?? []).length > 0 ? (
          <div className="flex flex-wrap gap-1 pt-1" aria-label="Tags">
            {entry.tags.map((tag) => (
              <span
                key={tag}
                className="inline-flex items-center rounded bg-surface-raised px-1.5 py-0.5 font-mono text-2xs text-fg-muted border border-border"
              >
                #{tag}
              </span>
            ))}
          </div>
        ) : null}
      </div>

      {/* Full body markdown content */}
      <div className="rounded-lg border border-border bg-surface-raised p-4">
        <h4 className="mb-2 text-xs font-semibold uppercase tracking-wider text-fg-muted">
          Content
        </h4>
        <div className="font-mono text-xs leading-relaxed whitespace-pre-wrap text-fg">
          {entry.body_markdown}
        </div>
      </div>

      {/* Revision History */}
      <div className="flex flex-col gap-3">
        <div className="flex items-center justify-between">
          <h4 className="flex items-center gap-1.5 text-sm font-semibold text-fg">
            <History className="size-4 text-fg-muted" aria-hidden="true" />
            Revision History
          </h4>
          {pruned > 0 ? (
            <span className="text-2xs text-fg-muted">({pruned} older revisions pruned)</span>
          ) : null}
        </div>

        {revisionsQuery.isLoading ? (
          <p className="text-xs text-fg-muted">Loading history…</p>
        ) : null}

        {revisionsQuery.isError ? (
          <p role="alert" className="text-xs text-danger">
            Could not load revision history.
          </p>
        ) : null}

        {revisions.length > 0 ? (
          <ol className="divide-y divide-border rounded-md border border-border bg-surface">
            {revisions.map((rev) => {
              const revWriter = formatMemoryWriter(
                { kind: rev.actor_kind, actor: rev.actor },
                undefined,
                nameOf,
              );
              const canRestoreThis = canMutate && rev.revision < entry.revision;

              return (
                <li
                  key={rev.revision}
                  className="flex flex-wrap items-center justify-between gap-2 px-3 py-2.5 text-xs"
                >
                  <div className="flex flex-col gap-0.5">
                    <div className="flex items-center gap-2">
                      <span className="font-mono font-medium text-fg">r{rev.revision}</span>
                      <span className="rounded bg-surface-raised px-1.5 py-0.2 font-mono text-2xs uppercase text-fg-muted border border-border">
                        {rev.action}
                      </span>
                      <span className="font-medium text-fg">{revWriter.label}</span>
                    </div>
                    {rev.at ? (
                      <span
                        className="text-2xs text-fg-muted"
                        title={new Date(rev.at).toLocaleString()}
                      >
                        {formatRelativeTime(rev.at)}
                      </span>
                    ) : null}
                  </div>

                  {canRestoreThis ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-7 text-xs"
                      disabled={patchMutation.isPending}
                      onClick={() => handleAction({ action: 'restore', revision: rev.revision })}
                      title={`Restore to revision ${rev.revision}`}
                    >
                      <RotateCcw className="mr-1 size-3" aria-hidden="true" />
                      Restore
                    </Button>
                  ) : null}
                </li>
              );
            })}
          </ol>
        ) : !revisionsQuery.isLoading ? (
          <p className="text-xs text-fg-muted">No revision records found.</p>
        ) : null}

        {revisionsQuery.hasNextPage ? (
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void revisionsQuery.fetchNextPage()}
            disabled={revisionsQuery.isFetchingNextPage}
            className="self-start"
          >
            {revisionsQuery.isFetchingNextPage ? 'Loading…' : 'Load more revisions'}
          </Button>
        ) : null}
      </div>

      {/* Edit dialog */}
      <EditMemoryDialog
        open={editOpen}
        onOpenChange={setEditOpen}
        runbookId={runbookId}
        projectId={projectId}
        entry={entry}
        onUpdated={onEntryUpdated}
        onReload={onReload}
      />
    </div>
  );
}
