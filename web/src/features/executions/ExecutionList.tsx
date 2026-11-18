// Execution list (M3): URL-synced filters, D22 cursor paging via the
// A6 table's Load more, rows link to the live document. A runbook's Runs
// tab reuses this with the runbook filter locked.
import { useNavigate } from '@tanstack/react-router';
import { ApiError } from '../../api/client';
import { useRunbooks } from '../../api/hooks';
import { useDirectoryNames } from '../context/names';
import { useOrgContext } from '../context/OrgContext';
import { ExecutionTable, FilterBar } from '../../ui/lists';
import { EmptyState, ErrorState, Skeleton } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import type { ExecutionFilters } from './filters';
import { toTableRow, useExecutionSearch } from './hooks';

export interface ExecutionListProps {
  projectRef: string;
  filters: ExecutionFilters;
  onFiltersChange(filters: ExecutionFilters): void;
  /** Set by the Runs tab: the runbook filter is fixed and hidden. */
  lockedRunbook?: string;
}

export function ExecutionList({
  projectRef,
  filters,
  onFiltersChange,
  lockedRunbook,
}: ExecutionListProps) {
  const navigate = useNavigate();
  const names = useDirectoryNames();
  const { resolveProject } = useOrgContext();
  const project = resolveProject(projectRef);
  const effective: ExecutionFilters = lockedRunbook
    ? { ...filters, runbook: lockedRunbook }
    : filters;
  const search = useExecutionSearch(project?.id, effective);
  const catalog = useRunbooks(project?.id);
  const slugById = new Map(
    (catalog.data ?? []).map((rb) => [rb.runbook_id, { slug: rb.slug, title: rb.title }]),
  );

  // The server resolves the runbook filter as an exact slug or ID (U20):
  // the filter picks from the catalog, and a filter naming no runbook (a
  // stale link) is a 404 that means "no such runbook", not a failed load.
  const runbookOptions = (catalog.data ?? []).map((rb) => ({
    value: rb.slug,
    label: rb.title ?? rb.slug,
  }));
  const unknownRunbook =
    Boolean(effective.runbook) && search.error instanceof ApiError && search.error.status === 404;

  const rows = (search.data?.pages ?? [])
    .flatMap((page) => page.items)
    .map((entry) => toTableRow(entry, slugById));

  return (
    <div className="grid gap-3">
      <FilterBar
        statuses={filters.statuses}
        onStatusChange={(statuses) => onFiltersChange({ ...filters, statuses })}
        runbook={lockedRunbook ?? filters.runbook}
        onRunbookChange={
          lockedRunbook ? undefined : (runbook) => onFiltersChange({ ...filters, runbook })
        }
        runbookOptions={lockedRunbook ? undefined : runbookOptions}
        tags={filters.tags}
        onTagsChange={(tags) => onFiltersChange({ ...filters, tags })}
        onReset={() => onFiltersChange({ statuses: [], runbook: '', tags: [] })}
      />
      {search.isLoading ? (
        <div aria-busy="true" className="grid gap-2">
          <p role="status" className="sr-only">
            Loading executions…
          </p>
          <Skeleton className="h-24 w-full" />
        </div>
      ) : null}
      {unknownRunbook ? (
        <EmptyState
          title={`No runbook “${effective.runbook}” in this project`}
          description="It may have been renamed or removed."
          action={
            lockedRunbook ? undefined : (
              <Button
                variant="secondary"
                onClick={() => onFiltersChange({ ...filters, runbook: '' })}
              >
                Show all runbooks
              </Button>
            )
          }
        />
      ) : null}
      {search.isError && !unknownRunbook ? (
        <ErrorState
          message="Executions could not be loaded."
          code={search.error instanceof ApiError ? search.error.code : undefined}
          requestId={
            search.error instanceof ApiError ? (search.error.requestId ?? undefined) : undefined
          }
          onRetry={() => void search.refetch()}
        />
      ) : null}
      {search.data ? (
        <ExecutionTable
          executions={rows}
          nameOf={names.nameOf}
          hasMore={Boolean(search.hasNextPage)}
          isLoadingMore={search.isFetchingNextPage}
          onLoadMore={() => void search.fetchNextPage()}
          onSelectExecution={(execution) =>
            void navigate({ to: '/x/$exe', params: { exe: execution.id } })
          }
        />
      ) : null}
    </div>
  );
}
