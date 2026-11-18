import { keepPreviousData, useInfiniteQuery, useQuery } from '@tanstack/react-query';
import { api, ApiError, type Schemas } from '../../api/client';
import { queryKeys } from '../../api/query-keys';
import { encodeTagParam, normalizeRow, uniqueTags, type RunbookRow } from './catalog';

/** A catalog row as this list reads it (run data may be withheld; see RunbookRow). */
export type RunbookCatalogView = RunbookRow;
export type VersionViewData = Schemas['VersionView'];
export type VersionDiff = Schemas['VersionDiffView'];
export type VersionSource = Schemas['VersionSourceView'];

interface Page<Item> {
  items: Item[];
  next_cursor?: string | null;
}

export interface CatalogQuery {
  /** Server-side search over title, slug, description and tags (UI-1). */
  q?: string;
  /** Server-side: runbooks carrying all of these tags (UI-1). */
  tags?: string[];
}

/** Runbooks a catalog request asks for. */
const PAGE = 25;
/** Extra requests one search page may make to fill itself (see below). */
const SEARCH_FOLLOWS = 4;

export function useRunbookCatalog(projectId: string | undefined, query: CatalogQuery = {}) {
  const q = query.q?.trim() ?? '';
  const tags = uniqueTags(query.tags).sort();
  return useInfiniteQuery({
    queryKey: [...queryKeys.runbooks(projectId ?? ''), 'paged', { q, tags }],
    enabled: Boolean(projectId),
    initialPageParam: undefined as string | undefined,
    // Keep the rows on screen while a new search loads (no flash to skeletons).
    placeholderData: keepPreviousData,
    queryFn: async ({ pageParam, signal }) => {
      const fetchPage = async (cursor: string | undefined) => {
        const { data } = await api.GET('/v1/projects/{p}/runbooks', {
          // UI-1c: a new keystroke cancels this query and the cursor
          // follows below with it.
          signal,
          params: {
            path: { p: projectId as string },
            query: {
              limit: PAGE,
              ...(cursor ? { cursor } : {}),
              ...(q ? { q } : {}),
              ...(tags.length ? { tag: encodeTagParam(tags) } : {}),
            },
          },
        });
        return data as Page<RunbookRow>;
      };
      let page = await fetchPage(pageParam);
      // UI-1b review round 1: the Workers edition examines a bounded number
      // of not-yet-listed runbooks a request, so a search can answer a short
      // (even empty) page with a cursor to continue from. Follow it a few
      // times so a search does not look finished while more may match.
      for (let follows = 0; follows < SEARCH_FOLLOWS; follows++) {
        if (!(q || tags.length) || page.items.length >= PAGE || !page.next_cursor) break;
        const next = await fetchPage(page.next_cursor);
        page = { items: [...page.items, ...next.items], next_cursor: next.next_cursor };
      }
      return { ...page, items: page.items.map(normalizeRow) };
    },
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
}

/**
 * UI-1c: the project's runbook with exactly this slug, read directly
 * (`GET /v1/projects/{p}/runbooks/{slug}`), not by walking the list.
 * Null when there is none (404).
 */
export async function findRunbookBySlug(
  projectId: string,
  slug: string,
  signal?: AbortSignal,
): Promise<RunbookRow | null> {
  try {
    const { data } = await api.GET('/v1/projects/{p}/runbooks/{slug}', {
      params: { path: { p: projectId, slug } },
      signal,
    });
    return normalizeRow(data as RunbookRow);
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return null;
    throw error;
  }
}

export function useVersions(runbookId: string | undefined) {
  return useInfiniteQuery({
    queryKey: [...queryKeys.runbookVersions(runbookId ?? ''), 'paged'],
    enabled: Boolean(runbookId),
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam }) => {
      const { data } = await api.GET('/v1/runbooks/{rb}/versions', {
        params: {
          path: { rb: runbookId as string },
          query: { limit: 25, ...(pageParam ? { cursor: pageParam } : {}) },
        },
      });
      return data as Page<VersionViewData>;
    },
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
}

export function versionRef(version: VersionViewData): string {
  return version.runbook_version != null
    ? String(version.runbook_version)
    : version.runbook_version_id;
}

export function useVersionDiff(runbookId: string, a: string | null, b: string | null) {
  return useQuery({
    queryKey: ['runbooks', runbookId, 'diff', a, b],
    enabled: Boolean(a && b && a !== b),
    queryFn: async () => {
      const { data } = await api.GET('/v1/runbooks/{rb}/versions/{a}/diff/{b}', {
        params: { path: { rb: runbookId, a: a as string, b: b as string } },
      });
      return data as VersionDiff;
    },
  });
}

export function useVersionSource(runbookId: string, version: string | null) {
  return useQuery({
    queryKey: ['runbooks', runbookId, 'versions', version, 'source'],
    enabled: Boolean(version),
    queryFn: async () => {
      const { data } = await api.GET('/v1/runbooks/{rb}/versions/{version}', {
        params: { path: { rb: runbookId, version: version as string } },
      });
      return data as VersionSource;
    },
  });
}

/**
 * Review C9: runbook routes accept a slug as well as an ID (the overview
 * always could; versions/editor called ID-only routes with it and 404'd).
 * Resolves a route ref to the runbook ID, or reports it missing.
 */
const noop = () => {};

export function useResolvedRunbookId(projectId: string | undefined, ref: string) {
  const isId = ref.startsWith('rb_');
  const query = useQuery({
    queryKey: ['runbooks', projectId, 'by-slug', ref],
    enabled: Boolean(projectId) && !isId,
    queryFn: ({ signal }) => findRunbookBySlug(projectId as string, ref, signal),
  });
  if (isId) return { runbookId: ref, pending: false, missing: false, failed: false, retry: noop };
  return {
    runbookId: query.data?.runbook_id,
    // Pending also while the project is unknown (the query waits for it):
    // "not found" only once the lookup answered.
    pending: query.isPending && !query.isError,
    missing: query.isSuccess && query.data === null,
    failed: query.isError,
    retry: () => void query.refetch(),
  };
}
