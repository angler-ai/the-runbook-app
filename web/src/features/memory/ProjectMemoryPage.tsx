import { useCallback, useEffect, useMemo, useState } from 'react';
import { useParams } from '@tanstack/react-router';
import { Brain, BookOpen, Folder, Lock, Plus, Search, X } from 'lucide-react';
import { useMe, useRunbooks } from '../../api/hooks';
import { formatRelativeTime } from '../../lib/format';
import {
  EmptyState,
  ErrorState,
  ListPaneHeading,
  ListPaneItem,
  ListPaneItems,
  PageHeader,
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
import { useOrgContext } from '../context/OrgContext';
import { useDirectoryNames } from '../context/names';
import { CreateMemoryDialog } from './CreateMemoryDialog';
import { MemoryDetail } from './MemoryDetail';
import { formatMemoryWriter, useProjectMemory, type MemoryEntry, type MemoryStatus } from './hooks';

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return debounced;
}

export function ProjectMemoryPage({ project: propProject }: { project?: string }) {
  let paramProject: string | undefined;
  try {
    const params = useParams({ strict: false }) as { project?: string };
    paramProject = params?.project;
  } catch {
    // Router context fallback for tests
  }
  const projectParam = propProject ?? paramProject;

  const { projects } = useOrgContext();
  const currentProject = useMemo(
    () => projects.find((p) => p.id === projectParam || p.slug === projectParam),
    [projects, projectParam],
  );

  const projectId = currentProject?.id;
  const me = useMe();
  const { nameOf } = useDirectoryNames();

  const isPerson = me.data?.credential ? me.data.credential.actor_kind !== 'agent' : true;
  const hasWriteScope = me.data?.credential?.scopes.includes('runbooks:write') ?? false;
  const canMutate = Boolean(hasWriteScope && isPerson);

  const [searchInput, setSearchInput] = useState('');
  const debouncedSearch = useDebounced(searchInput, 250);
  const [statusFilter, setStatusFilter] = useState<MemoryStatus>('active');
  const [runbookFilter, setRunbookFilter] = useState<string>('all');
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);

  const runbooksQuery = useRunbooks(projectId);
  const runbooks = useMemo(() => runbooksQuery.data ?? [], [runbooksQuery.data]);
  const runbookMap = useMemo(() => new Map(runbooks.map((rb) => [rb.runbook_id, rb])), [runbooks]);

  const memoryQuery = useProjectMemory(projectId, {
    q: debouncedSearch,
    status: statusFilter,
  });

  const allEntries = useMemo(
    () => (memoryQuery.data?.pages ?? []).flatMap((p) => p.items),
    [memoryQuery.data],
  );

  const filteredEntries = useMemo(() => {
    if (runbookFilter === 'all') return allEntries;
    if (runbookFilter === 'project') {
      return allEntries.filter((e) => e.scope.kind === 'project');
    }
    return allEntries.filter(
      (e) =>
        e.scope.kind === 'runbook' &&
        (e.scope.runbook_id === runbookFilter ||
          runbookMap.get(e.scope.runbook_id)?.slug === runbookFilter),
    );
  }, [allEntries, runbookFilter, runbookMap]);

  const selectedEntry = useMemo(() => {
    if (!filteredEntries.length) return null;
    if (selectedId) {
      const found = filteredEntries.find((e) => e.id === selectedId);
      if (found) return found;
    }
    return filteredEntries[0] ?? null;
  }, [filteredEntries, selectedId]);

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

  const scopeBadge = (entry: MemoryEntry) => {
    if (entry.scope.kind === 'runbook') {
      const rb = runbookMap.get(entry.scope.runbook_id);
      const name = rb?.title ?? rb?.slug ?? entry.scope.runbook_id;
      return (
        <span className="inline-flex items-center gap-1 rounded bg-surface-raised px-1.5 py-0.5 font-mono text-2xs text-fg-muted border border-border/60">
          <BookOpen className="size-3" aria-hidden="true" />
          <span className="max-w-[120px] truncate">{name}</span>
        </span>
      );
    }
    return (
      <span className="inline-flex items-center gap-1 rounded bg-surface-raised px-1.5 py-0.5 font-mono text-2xs text-fg-muted border border-border/60">
        <Folder className="size-3" aria-hidden="true" />
        <span>Project</span>
      </span>
    );
  };

  if (!currentProject) {
    return (
      <div className="">
        <PageHeader title="Memory" description="Project not found." />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-6" data-slot="project-memory-page">
      <PageHeader
        title={`${currentProject.title} memory`}
        description={`Project-wide and runbook memory entries for ${currentProject.title} (${currentProject.slug}).`}
      />

      {/* Filters & Actions toolbar */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-1 flex-wrap items-center gap-2 sm:max-w-xl">
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

          <div className="w-44">
            <Select value={runbookFilter} onValueChange={setRunbookFilter}>
              <SelectTrigger aria-label="Filter by runbook">
                <SelectValue placeholder="All runbooks" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All runbooks</SelectItem>
                <SelectItem value="project">Project memory only</SelectItem>
                {runbooks.map((rb) => (
                  <SelectItem key={rb.runbook_id} value={rb.runbook_id}>
                    {rb.title || rb.slug}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>

        {canMutate ? (
          <Button onClick={() => setCreateOpen(true)} className="gap-1.5">
            <Plus className="size-4" aria-hidden="true" />
            <span>New memory</span>
          </Button>
        ) : null}
      </div>

      {/* Main Content */}
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

      {!memoryQuery.isLoading && !memoryQuery.isError && filteredEntries.length === 0 ? (
        <EmptyState
          icon={Brain}
          title={
            searchInput || runbookFilter !== 'all'
              ? 'No matching memory entries'
              : 'No memory entries yet'
          }
          description={
            searchInput || runbookFilter !== 'all'
              ? 'No memory entries match the current filters. Try adjusting your query or filters.'
              : 'Save run context, operating facts and learned knowledge for this project.'
          }
          action={
            searchInput || runbookFilter !== 'all' ? (
              <Button
                variant="secondary"
                onClick={() => {
                  setSearchInput('');
                  setRunbookFilter('all');
                }}
              >
                Clear filters
              </Button>
            ) : canMutate ? (
              <Button onClick={() => setCreateOpen(true)}>
                <Plus className="mr-1.5 size-4" aria-hidden="true" />
                New memory
              </Button>
            ) : undefined
          }
        />
      ) : null}

      {!memoryQuery.isLoading && !memoryQuery.isError && filteredEntries.length > 0 ? (
        <SplitPane
          ariaLabel="Project memory list and details"
          fillViewport
          leftWidth="md:w-80 lg:w-96"
          leftClassName="p-3 sm:p-4"
          left={
            <section aria-labelledby="memory-list-heading">
              <ListPaneHeading
                id="memory-list-heading"
                title="Entries"
                count={filteredEntries.length}
                more={Boolean(memoryQuery.hasNextPage)}
              />
              <ListPaneItems label="Memory entries">
                {filteredEntries.map((entry) => {
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
                      detail={
                        <span className="flex items-center gap-1.5">
                          {scopeBadge(entry)}
                          <span className="truncate">By {writer.label}</span>
                          {entry.updated_at ? (
                            <span className="shrink-0">
                              · {formatRelativeTime(entry.updated_at)}
                            </span>
                          ) : null}
                        </span>
                      }
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
                  {memoryQuery.isFetchingNextPage ? 'Loading more…' : 'Load more'}
                </Button>
              ) : null}
            </section>
          }
          right={
            selectedEntry ? (
              <div className="flex flex-col gap-4">
                {selectedEntry.scope.kind === 'runbook' ? (
                  <div className="flex items-center gap-1.5 rounded-md border border-border bg-surface-raised/40 px-3 py-2 text-xs text-fg-muted">
                    <BookOpen className="size-3.5" aria-hidden="true" />
                    <span>Runbook entry for</span>
                    <a
                      href={`/p/${currentProject.slug}/runbooks/${runbookMap.get(selectedEntry.scope.runbook_id)?.slug ?? selectedEntry.scope.runbook_id}?tab=memory`}
                      className="font-medium text-fg underline hover:text-accent"
                    >
                      {runbookMap.get(selectedEntry.scope.runbook_id)?.title ??
                        selectedEntry.scope.runbook_id}
                    </a>
                  </div>
                ) : (
                  <div className="flex items-center gap-1.5 rounded-md border border-border bg-surface-raised/40 px-3 py-2 text-xs text-fg-muted">
                    <Folder className="size-3.5" aria-hidden="true" />
                    <span>Project-level memory entry</span>
                  </div>
                )}
                <MemoryDetail
                  key={selectedEntry.id}
                  entry={selectedEntry}
                  projectId={projectId}
                  runbookId={
                    selectedEntry.scope.kind === 'runbook'
                      ? selectedEntry.scope.runbook_id
                      : undefined
                  }
                  canMutate={canMutate}
                  nameOf={nameOf}
                  onEntryUpdated={handleUpdated}
                  onReload={() => void memoryQuery.refetch()}
                />
              </div>
            ) : (
              <div className="flex h-48 items-center justify-center text-sm text-fg-muted">
                Select an entry to view details.
              </div>
            )
          }
        />
      ) : null}

      <CreateMemoryDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        projectId={projectId}
        onCreated={handleCreated}
      />
    </div>
  );
}
