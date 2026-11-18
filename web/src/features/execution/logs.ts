// A step's log as the run page shows it: fetched when first opened, paged,
// and, whenever a live event refreshes the document while the step may
// still be writing to it (U17), extended with the lines after the last one
// loaded (the server's cursor is the last sequence read, so it resumes
// there). Pages already read are never fetched again.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useQueryClient, type InfiniteData } from '@tanstack/react-query';
import { api, ApiError } from '../../api/client';
import { stepLogsKey, useStepLogs, type StepLogPage, type StepSectionData } from './hooks';

/** Steps whose log may still grow. */
const LOG_ACTIVE = new Set(['claimed', 'waiting']);
/** Copy loads at most this many pages (of 200 lines) beyond those read. */
const COPY_PAGE_CAP = 100;

export interface LogView {
  lines: string[];
  loading: boolean;
  error: string | null;
  hasMore: boolean;
  loadMore: () => void;
  /** Start reading the log (the block opened). */
  open: () => void;
  /** Every line, loading the pages not read yet; `complete` false when capped. */
  copyAll: () => Promise<{ lines: string[]; complete: boolean }>;
}

type Pages = InfiniteData<StepLogPage, unknown>;

const linesOf = (data: Pages | undefined) =>
  (data?.pages ?? []).flatMap((page) => page.lines.map((entry) => entry.line));

export function useLogView(section: StepSectionData, documentAt: number): LogView {
  const [wanted, setWanted] = useState(false);
  const stepRunId = section.step_run_id;
  const logs = useStepLogs(stepRunId, wanted);
  const queryClient = useQueryClient();
  const expired = logs.error instanceof ApiError && logs.error.status === 410;
  const lines = useMemo(
    () => (expired ? ['Log history has expired for this step.'] : linesOf(logs.data)),
    [logs.data, expired],
  );
  const error =
    logs.error && !expired
      ? `The log could not be loaded${logs.error instanceof ApiError ? ` (${logs.error.code})` : ''}.`
      : null;

  /** Append the lines written since the last page read. */
  const tail = useCallback(async () => {
    const key = stepLogsKey(stepRunId);
    const data = queryClient.getQueryData<Pages>(key);
    const last = data?.pages[data.pages.length - 1];
    // Nothing read yet, or the person has pages left to load: nothing to tail.
    if (!last || last.has_more) return;
    const { data: page } = await api.GET('/v1/step-runs/{sr}/logs', {
      params: { path: { sr: stepRunId }, query: { cursor: last.next_cursor, limit: 200 } },
    });
    const next = page as StepLogPage | undefined;
    if (!next || next.lines.length === 0) return;
    queryClient.setQueryData<Pages>(key, (old) =>
      old && old.pages[old.pages.length - 1] === last
        ? { pages: [...old.pages, next], pageParams: [...old.pageParams, last.next_cursor] }
        : old,
    );
  }, [queryClient, stepRunId]);

  const lastStatus = useRef(section.status);
  useEffect(() => {
    const wasActive = LOG_ACTIVE.has(lastStatus.current);
    lastStatus.current = section.status;
    if (wanted && (wasActive || LOG_ACTIVE.has(section.status))) {
      tail().catch(() => {
        // A failed tail leaves the lines read so far; the next refresh retries.
      });
    }
    // Only a new document read should trigger this.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [documentAt]);

  const { fetchNextPage } = logs;
  const copyAll = useCallback(async () => {
    let result = await fetchNextPage({ cancelRefetch: false }).catch(() => null);
    let pages = 0;
    while (result?.hasNextPage && pages < COPY_PAGE_CAP) {
      result = await fetchNextPage({ cancelRefetch: false }).catch(() => null);
      pages += 1;
    }
    const data = queryClient.getQueryData<Pages>(stepLogsKey(stepRunId));
    const last = data?.pages[data.pages.length - 1];
    return { lines: linesOf(data), complete: !last?.has_more };
  }, [fetchNextPage, queryClient, stepRunId]);

  const open = useMemo(() => () => setWanted(true), []);
  return {
    lines,
    loading: logs.isLoading,
    error,
    hasMore: Boolean(logs.hasNextPage),
    loadMore: () => void logs.fetchNextPage(),
    open,
    copyAll,
  };
}
