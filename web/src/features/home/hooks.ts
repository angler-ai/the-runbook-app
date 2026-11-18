// UI-2: the Home page's reads. Each section reads existing /v1 routes on its
// own query, so one failing read leaves the rest of the page standing:
// - Needs you: the shared inbox (`/v1/me/tasks`, features/inbox/hooks).
// - Running now / Failed: `GET /v1/projects/{p}/executions?status=…&limit=5`
//   (Failed: started in the last 7 days, `&from=`), then each listed run's
//   document (`/v1/executions/{exe}/report`, the same query key the run
//   page reads, so opening a run is instant; a finished run's is not polled).
// - Health and Activity: the runbook catalog (`useRunbooks`) and the runs
//   created in the last 7 days (`?from=`), at most HOME_RECENT_PAGES pages.
import { useQueries, useQuery, type Query } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
import { api, type Schemas } from '../../api/client';
import { queryKeys } from '../../api/query-keys';
import { TERMINAL_STATUSES, type RunDocument, type RunEntry } from './model';

export const HOME_POLL_MS = 30_000;
export const HOME_LIST_LIMIT = 5;
/** 3 pages of 100: a week of up to 300 runs; past that the sparkline hides. */
export const HOME_RECENT_PAGES = 3;
export const HOME_WINDOW_DAYS = 7;

/**
 * Placeholder data across a key change: the last data of the same
 * project's query (a new day's window), never another project's.
 */
function sameProject<T>(projectId: string | undefined) {
  return (previous: T | undefined, query: Query<T, Error, T, readonly unknown[]> | undefined) =>
    query && query.queryKey[1] === (projectId ?? '') ? previous : undefined;
}

/**
 * The page's clock: `fixed` when given (the gallery), else the time now,
 * re-read every HOME_POLL_MS and whenever the tab comes back into view, so
 * ages advance and "today" turns over at local midnight.
 */
export function useNow(fixed?: number): number {
  const [now, setNow] = useState(() => fixed ?? Date.now());
  useEffect(() => {
    if (fixed != null) return;
    const tick = () => setNow(Date.now());
    const timer = setInterval(tick, HOME_POLL_MS);
    const onVisible = () => {
      if (document.visibilityState === 'visible') tick();
    };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      clearInterval(timer);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, [fixed]);
  return fixed ?? now;
}

/** Runs in `statuses`, newest first, top HOME_LIST_LIMIT; started since `fromMs` when given. */
export function useRunsWithStatus(
  projectId: string | undefined,
  statuses: readonly string[],
  fromMs?: number,
) {
  return useQuery({
    queryKey: ['projects', projectId ?? '', 'home', 'runs', statuses.join(','), fromMs ?? null],
    enabled: Boolean(projectId),
    refetchInterval: HOME_POLL_MS,
    // A new window at midnight keeps the old list on screen until it loads.
    placeholderData: sameProject<Schemas['RunSearchPage']>(projectId),
    queryFn: async () => {
      const { data } = await api.GET('/v1/projects/{p}/executions', {
        params: {
          path: { p: projectId as string },
          query: {
            status: statuses.join(','),
            limit: String(HOME_LIST_LIMIT),
            ...(fromMs != null ? { from: String(fromMs) } : {}),
          },
        },
      });
      return data as Schemas['RunSearchPage'];
    },
  });
}

/**
 * The documents of `ids`, in order; each entry is its own query, polled
 * until its run is finished (a finished run's document no longer changes).
 */
export function useRunDocuments(ids: readonly string[]) {
  return useQueries({
    queries: ids.map((id) => ({
      queryKey: queryKeys.executionDocument(id),
      refetchInterval: (query: { state: { data?: RunDocument } }) =>
        TERMINAL_STATUSES.has(query.state.data?.header.status ?? '') ? false : HOME_POLL_MS,
      queryFn: async () => {
        const { data } = await api.GET('/v1/executions/{exe}/report', {
          params: { path: { exe: id } },
        });
        return data as RunDocument;
      },
    })),
  });
}

export interface RecentRuns {
  entries: RunEntry[];
  /** Every run since `fromMs` was read (no page cap reached). */
  complete: boolean;
  fromMs: number;
}

/** Runs created since `fromMs`, newest first. */
export function useRecentRuns(projectId: string | undefined, fromMs: number) {
  return useQuery({
    queryKey: ['projects', projectId ?? '', 'home', 'recent', fromMs],
    enabled: Boolean(projectId),
    refetchInterval: HOME_POLL_MS,
    placeholderData: sameProject<RecentRuns>(projectId),
    queryFn: async (): Promise<RecentRuns> => {
      const entries: RunEntry[] = [];
      let cursor: string | undefined;
      for (let page = 0; page < HOME_RECENT_PAGES; page += 1) {
        const { data } = await api.GET('/v1/projects/{p}/executions', {
          params: {
            path: { p: projectId as string },
            query: { from: String(fromMs), limit: '100', ...(cursor ? { cursor } : {}) },
          },
        });
        const result = data as Schemas['RunSearchPage'];
        entries.push(...result.items);
        if (!result.next_cursor) return { entries, complete: true, fromMs };
        cursor = result.next_cursor;
      }
      return { entries, complete: false, fromMs };
    },
  });
}
