// A runbook's runs as a list beside the selected run (like Webhooks): the
// left pane names each run (what tells it apart, its status, when and who
// started it), the right pane is the run itself. The first run is selected
// until another is picked; the choice is in the URL (`?run=`). On a phone
// the list is the page and a run opens on its own page.
import { Link, useNavigate } from '@tanstack/react-router';
import { ExternalLink, PlayCircle } from 'lucide-react';
import { useState } from 'react';
import { ApiError } from '../../api/client';
import { formatRelativeTime } from '../../lib/format';
import {
  EmptyState,
  ErrorState,
  ListPaneHeading,
  ListPaneItem,
  ListPaneItems,
  Skeleton,
  SplitPane,
} from '../../ui/layout';
import { FilterBar, startedBy, triggerLabel } from '../../ui/lists';
import { Button } from '../../ui/primitives/button';
import { StatusPill } from '../../ui/status/StatusPill';
import { useDirectoryNames } from '../context/names';
import { useOrgContext } from '../context/OrgContext';
import { ExecutionDocumentPage } from '../execution/ExecutionDocumentPage';
import { useRunIdentities } from '../execution/runIdentity';
import type { RunView } from '../execution/views';
import { useNow } from '../home/hooks';
import type { ExecutionFilters } from './filters';
import { useExecutionSearch } from './hooks';

/** Below md the split stacks; a run then opens on its own page. */
const NARROW = '(max-width: 767px)';

export interface RunbookRunsProps {
  projectRef: string;
  /** The runbook (ID or slug): the list's fixed filter. */
  runbook: string;
  /** Its title: a run named just that is told apart by its inputs instead. */
  runbookTitle?: string | null;
  filters: ExecutionFilters;
  onFiltersChange(filters: ExecutionFilters): void;
  /** The run picked in the URL; the first run otherwise. */
  selected?: string;
  onSelect(executionId: string): void;
}

/** Who started a run, in words: "Webhook new_customer", "Jordan", "Service token". */
function starter(
  createdBy: string | null | undefined,
  nameOf: (userId: string) => string,
): string | undefined {
  const trigger = triggerLabel(createdBy);
  if (trigger) return trigger;
  const actor = startedBy(createdBy, nameOf);
  if (!actor) return undefined;
  if (actor.kind === 'human') return nameOf(actor.user_id);
  if (actor.kind === 'system') return 'System';
  return actor.name;
}

export function RunbookRuns({
  projectRef,
  runbook,
  runbookTitle,
  filters,
  onFiltersChange,
  selected,
  onSelect,
}: RunbookRunsProps) {
  const navigate = useNavigate();
  const names = useDirectoryNames();
  const { resolveProject } = useOrgContext();
  const project = resolveProject(projectRef);
  const search = useExecutionSearch(project?.id, { ...filters, runbook });
  const entries = (search.data?.pages ?? []).flatMap((page) => page.items);
  const identities = useRunIdentities(
    entries.map((entry) => entry.id),
    names,
  );
  const now = useNow();
  const [view, setView] = useState<RunView>('graph');
  const current = selected ?? entries[0]?.id;

  function pick(id: string) {
    if (globalThis.matchMedia?.(NARROW).matches) {
      void navigate({ to: '/x/$exe', params: { exe: id } });
      return;
    }
    onSelect(id);
  }

  const filterBar = (
    <FilterBar
      statuses={filters.statuses}
      onStatusChange={(statuses) => onFiltersChange({ ...filters, statuses })}
      tags={filters.tags}
      onTagsChange={(tags) => onFiltersChange({ ...filters, tags })}
      onReset={() => onFiltersChange({ statuses: [], runbook: '', tags: [] })}
    />
  );

  if (search.isLoading) {
    return (
      <div className="grid gap-3">
        {filterBar}
        <div aria-busy="true" className="grid gap-2">
          <p role="status" className="sr-only">
            Loading runs…
          </p>
          <Skeleton className="h-64 w-full" />
        </div>
      </div>
    );
  }
  if (search.isError) {
    return (
      <div className="grid gap-3">
        {filterBar}
        <ErrorState
          message="Runs could not be loaded."
          code={search.error instanceof ApiError ? search.error.code : undefined}
          requestId={
            search.error instanceof ApiError ? (search.error.requestId ?? undefined) : undefined
          }
          onRetry={() => void search.refetch()}
        />
      </div>
    );
  }
  if (entries.length === 0) {
    const filtered = filters.statuses.length > 0 || filters.tags.length > 0;
    return (
      <div className="grid gap-3">
        {filterBar}
        <EmptyState
          icon={PlayCircle}
          title={filtered ? 'No runs match these filters' : 'No runs yet'}
          description={
            filtered
              ? 'Clear the filters to see every run of this runbook.'
              : 'Runs of this runbook show up here once one starts.'
          }
        />
      </div>
    );
  }

  return (
    <div className="grid gap-3">
      {filterBar}
      <SplitPane
        ariaLabel="Runs"
        fillViewport
        leftWidth="md:w-80 lg:w-96"
        leftClassName="p-3 sm:p-4"
        rightClassName="hidden md:block"
        left={
          <section aria-labelledby="runs-list-heading">
            <ListPaneHeading
              id="runs-list-heading"
              title="Runs"
              count={entries.length}
              more={Boolean(search.hasNextPage)}
            />
            <ListPaneItems>
              {entries.map((entry) => {
                const identity = identities.get(entry.id);
                const named = entry.title && entry.title !== runbookTitle;
                const label = named ? entry.title : (identity?.summary ?? entry.title ?? 'Run');
                const who = starter(entry.created_by, names.nameOf);
                const detail = [
                  named && identity?.summary ? identity.summary : null,
                  formatRelativeTime(entry.created_at_ms, now),
                  who,
                ]
                  .filter(Boolean)
                  .join(' · ');
                return (
                  <ListPaneItem
                    key={entry.id}
                    slot="run-row"
                    selected={entry.id === current}
                    onSelect={() => pick(entry.id)}
                    title={label}
                    trailing={<StatusPill status={entry.status} />}
                    detail={detail}
                  />
                );
              })}
            </ListPaneItems>
            {search.hasNextPage ? (
              <Button
                variant="ghost"
                size="sm"
                onClick={() => void search.fetchNextPage()}
                disabled={search.isFetchingNextPage}
              >
                {search.isFetchingNextPage ? 'Loading…' : 'Load more'}
              </Button>
            ) : null}
          </section>
        }
        right={
          current ? (
            <div className="grid gap-3" data-testid="selected-run">
              <div className="flex justify-end">
                <Link
                  to="/x/$exe"
                  params={{ exe: current }}
                  className="inline-flex items-center gap-1 text-xs font-medium text-accent hover:underline"
                >
                  Open the run’s own page
                  <ExternalLink className="size-3.5" aria-hidden="true" />
                </Link>
              </div>
              <ExecutionDocumentPage
                key={current}
                executionId={current}
                view={view}
                onViewChange={setView}
              />
            </div>
          ) : null
        }
      />
    </div>
  );
}
