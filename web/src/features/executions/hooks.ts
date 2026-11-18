import { useInfiniteQuery } from '@tanstack/react-query';
import { api, type Schemas } from '../../api/client';
import type { ExecutionView as TableExecutionView } from '../../ui/lists';
import type { ExecutionFilters } from './filters';

export type RunSearchEntry = Schemas['RunSearchEntry'];
export type RunSearchPageData = Schemas['RunSearchPage'];

export const executionSearchKey = (projectId: string, filters: ExecutionFilters) =>
  [
    'projects',
    projectId,
    'executions',
    filters.statuses.join(','),
    filters.runbook,
    filters.tags.join(','),
  ] as const;

/** D22 cursor paging: cursors bind scope and filters server-side. */
export function useExecutionSearch(projectId: string | undefined, filters: ExecutionFilters) {
  return useInfiniteQuery({
    queryKey: executionSearchKey(projectId ?? '', filters),
    enabled: Boolean(projectId),
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam }) => {
      const { data } = await api.GET('/v1/projects/{p}/executions', {
        params: {
          path: { p: projectId as string },
          query: {
            limit: '25',
            ...(filters.statuses.length ? { status: filters.statuses.join(',') } : {}),
            ...(filters.runbook ? { runbook: filters.runbook } : {}),
            ...(filters.tags.length ? { tags: filters.tags.join(',') } : {}),
            ...(pageParam ? { cursor: pageParam } : {}),
          },
        },
      });
      return data as RunSearchPageData;
    },
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
}

/** The search entry is a summary; the table renders ExecutionView rows. */
export function toTableRow(
  entry: RunSearchEntry,
  runbookById: ReadonlyMap<string, { slug: string; title?: string | null }>,
): TableExecutionView {
  const runbook = runbookById.get(entry.runbook_id);
  return {
    schema_revision: 1,
    id: entry.id,
    runbook_id: entry.runbook_id,
    runbook_slug: runbook?.slug ?? null,
    runbook_title: runbook?.title ?? null,
    runbook_version_id: '',
    runbook_version: null,
    title: entry.title,
    status: entry.status,
    phase: 'ordinary',
    inputs: {},
    fields: entry.fields,
    tags: entry.tags,
    steps: [],
    amended: false,
    amendments: [],
    stale_step_ids: [],
    created_at_ms: entry.created_at_ms,
    created_by: entry.created_by,
    // The search entry has no start instant separate from creation.
    started_at_ms: entry.status === 'queued' ? null : entry.created_at_ms,
    completed_at_ms: entry.completed_at_ms ?? null,
  };
}
