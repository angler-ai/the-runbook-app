import { useCallback, useEffect, useMemo, useState } from 'react';
import { Brain, Lock, Plus, Search, X } from 'lucide-react';
import { formatRelativeTime } from '../../lib/format';
import { useMe } from '../../api/hooks';
import { useDirectoryNames } from '../context/names';
import {
  EmptyState,
  ErrorState,
  ListPaneHeading,
  ListPaneItem,
  ListPaneItems,
  Skeleton,
  SplitPane,
} from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '../../ui/primitives/select';
import { CreateMemoryDialog } from './CreateMemoryDialog';
import { MemoryDetail } from './MemoryDetail';
import { formatMemoryWriter, useRunbookMemory, type MemoryEntry, type MemoryStatus } from './hooks';

export interface RunbookMemoryTabProps {
  runbookId: string;
  projectId?: string;
  canWrite?: boolean;
}

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return debounced;
}

export function RunbookMemoryTab({
  runbookId,
  projectId: _projectId,
  canWrite,
}: RunbookMemoryTabProps) {
  const me = useMe();
  const { nameOf } = useDirectoryNames();

  const isPerson = me.data?.credential ? me.data.credential.actor_kind !== 'agent' : true;
  const hasWriteScope = me.data?.credential.scopes.includes('runbooks:write') ?? false;
  const userCanWrite = canWrite ?? hasWriteScope;
  const canMutate = Boolean(userCanWrite && isPerson);

  const [searchInput, setSearchInput] = useState('');
  const debouncedSearch = useDebounced(searchInput, 250);
  const [statusFilter, setStatusFilter] = useState<MemoryStatus>('active');
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);

  const memoryQuery = useRunbookMemory(runbookId, {
    q: debouncedSearch,
    status: statusFilter,
  });

  const entries = useMemo(
    () => (memoryQuery.data?.pages ?? []).flatMap((p) => p.items),
    [memoryQuery.data],
  );

  // If selectedId is null or not found in entries, select the first entry
  const selectedEntry = useMemo(() => {
    if (!entries.length) return null;
    if (selectedId) {
      const found = entries.find((e) => e.id === selectedId);
      if (found) return found;
    }
    return entries[0] ?? null;
  }, [entries, selectedId]);

  const handleCreated = useCallback((newEntry: MemoryEntry) => {
    setSelectedId(newEntry.id);
  }, []);

  const handleUpdated = useCallback((updated: MemoryEntry) => {
    setSelectedId(updated.id);
  }, []);

  const statusBadge = (entry: MemoryEntry) => {
    if (entry.status === 'proposed') {
      return (
        <span className="rounded bg-verdict-warn-bg px-1.5 py-0.5 text-2xs font-medium text-verdict-warn-fg border border-verdict-warn-border">
          Proposed
        </span>
      );
    }
    if (entry.status === 'archived') {
      return (
        <span className="rounded bg-surface-raised px-1.5 py-0.5 text-2xs font-medium text-fg-muted border border-border">
          Archived
        </span>
      );
    }
    return (
      <span className="rounded bg-verdict-pass-bg px-1.5 py-0.5 text-2xs font-medium text-verdict-pass-fg border border-verdict-pass-border">
        Active
      </span>
    );
  };

  return (
    <div className="flex flex-col gap-4" data-slot="runbook-memory-tab">
      {/* Search and Filters toolbar */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-1 flex-wrap items-center gap-2 sm:max-w-md">
          <div className="relative flex-1 min-w-[200px]">
            <Search
              className="absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted"
              aria-hidden="true"
            />
            <Input
              value={searchInput}
              onChange={(e) => setSearchInput(e.target.value)}
              placeholder="Search memory…"
              className="pl-9 pr-8"
              aria-label="Search memory entries"
            />
            {searchInput ? (
              <button
                type="button"
                onClick={() => setSearchInput('')}
                className="absolute right-2.5 top-1/2 -translate-y-1/2 text-fg-muted hover:text-fg"
                aria-label="Clear search"
              >
                <X className="size-4" aria-hidden="true" />
              </button>
            ) : null}
          </div>

          <div className="w-32">
            <Select
              value={statusFilter}
              onValueChange={(val) => setStatusFilter(val as MemoryStatus)}
            >
              <SelectTrigger aria-label="Filter by status">
                <SelectValue placeholder="Status" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="active">Active</SelectItem>
                <SelectItem value="proposed">Proposed</SelectItem>
                <SelectItem value="archived">Archived</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>

        {userCanWrite ? (
          <Button onClick={() => setCreateOpen(true)}>
            <Plus className="mr-1.5 size-4" aria-hidden="true" />
            New memory
          </Button>
        ) : null}
      </div>

      {/* Main content */}
      {memoryQuery.isLoading ? (
        <div className="grid gap-3 p-4" aria-busy="true">
          <p role="status" className="sr-only">
            Loading memory…
          </p>
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      ) : null}

      {memoryQuery.isError ? (
        <ErrorState
          message="Memory entries could not be loaded."
          onRetry={() => void memoryQuery.refetch()}
        />
      ) : null}

      {!memoryQuery.isLoading && !memoryQuery.isError && entries.length === 0 ? (
        <EmptyState
          icon={Brain}
          title={searchInput ? 'No matching memory entries' : 'No memory entries yet'}
          description={
            searchInput
              ? `No entries match "${searchInput}". Try another query or clear the filter.`
              : 'Save run context, operating facts and learned knowledge for this runbook.'
          }
          action={
            searchInput ? (
              <Button variant="secondary" onClick={() => setSearchInput('')}>
                Clear search
              </Button>
            ) : userCanWrite ? (
              <Button onClick={() => setCreateOpen(true)}>
                <Plus className="mr-1.5 size-4" aria-hidden="true" />
                New memory
              </Button>
            ) : undefined
          }
        />
      ) : null}

      {!memoryQuery.isLoading && !memoryQuery.isError && entries.length > 0 ? (
        <SplitPane
          ariaLabel="Memory list and details"
          fillViewport
          leftWidth="md:w-80 lg:w-96"
          leftClassName="p-3 sm:p-4"
          left={
            <section aria-labelledby="runbook-memory-heading">
              <ListPaneHeading
                id="runbook-memory-heading"
                title="Entries"
                count={entries.length}
                more={Boolean(memoryQuery.hasNextPage)}
              />
              <ListPaneItems label="Memory entries">
                {entries.map((entry) => {
                  const writer = formatMemoryWriter(entry.last_writer, entry.source, nameOf);
                  const displayTitle =
                    entry.title || entry.body_markdown.split('\n')[0] || entry.id;
                  return (
                    <ListPaneItem
                      key={entry.id}
                      selected={selectedEntry?.id === entry.id}
                      onSelect={() => setSelectedId(entry.id)}
                      title={displayTitle}
                      trailing={
                        <>
                          {statusBadge(entry)}
                          {entry.locked ? (
                            <Lock className="size-3 text-fg-muted" aria-label="Locked" />
                          ) : null}
                        </>
                      }
                      detail={`By ${writer.label}${
                        entry.updated_at ? ` · ${formatRelativeTime(entry.updated_at)}` : ''
                      }`}
                    />
                  );
                })}
              </ListPaneItems>
              {memoryQuery.hasNextPage ? (
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => void memoryQuery.fetchNextPage()}
                  disabled={memoryQuery.isFetchingNextPage}
                  className="mt-2 w-full"
                >
                  {memoryQuery.isFetchingNextPage ? 'Loading more…' : 'Load more entries'}
                </Button>
              ) : null}
            </section>
          }
          right={
            selectedEntry ? (
              <MemoryDetail
                key={selectedEntry.id}
                entry={selectedEntry}
                runbookId={runbookId}
                canMutate={canMutate}
                nameOf={nameOf}
                onEntryUpdated={handleUpdated}
                onReload={() => void memoryQuery.refetch()}
              />
            ) : (
              <div className="flex h-48 items-center justify-center text-sm text-fg-muted">
                Select a memory entry to view its details.
              </div>
            )
          }
        />
      ) : null}

      <CreateMemoryDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        runbookId={runbookId}
        onCreated={handleCreated}
      />
    </div>
  );
}
