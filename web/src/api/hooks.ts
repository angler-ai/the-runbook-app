// Per-resource query hooks over the shared client. The client middleware
// throws ApiError for problem responses, so query functions return data
// directly and React Query surfaces the ApiError as the query error.
import { useQuery } from '@tanstack/react-query';
import { api, type Schemas } from './client';
import { queryKeys } from './query-keys';
import { sessionStore, tokenStore } from './token-store';

export type MeView = Schemas['MeView'];
export type MetaView = Schemas['MetaView'];
export type DiscoveryView = Schemas['DiscoveryView'];
export type OrganizationView = Schemas['OrganizationView'];
export type ProjectView = Schemas['ProjectView'];
export type DirectoryUserView = Schemas['DirectoryUserView'];
export type TeamView = Schemas['TeamView'];

interface Page<Item> {
  items: Item[];
  next_cursor?: string | null;
}

/** Follow directory cursors to the end (bounded: pilot directories are small). */
export async function fetchAllPages<Item>(
  fetchPage: (cursor?: string) => Promise<Page<Item>>,
): Promise<Item[]> {
  const items: Item[] = [];
  let cursor: string | undefined;
  for (let page = 0; page < 50; page += 1) {
    const result = await fetchPage(cursor);
    items.push(...result.items);
    if (!result.next_cursor) return items;
    cursor = result.next_cursor;
  }
  return items;
}

export const signedIn = () => tokenStore.get() !== null || sessionStore.getKind() === 'cookie';

export function useDiscovery() {
  return useQuery({
    queryKey: ['discovery'],
    queryFn: async () => {
      try {
        const { data } = await api.GET('/.well-known/runbook');
        return data as DiscoveryView;
      } catch {
        return null;
      }
    },
    staleTime: 5 * 60 * 1000,
  });
}

export function useMeta() {
  return useQuery({
    queryKey: queryKeys.meta,
    queryFn: async () => {
      const { data } = await api.GET('/v1/meta');
      return data as MetaView;
    },
    staleTime: 5 * 60 * 1000,
  });
}


export function useMe() {
  return useQuery({
    queryKey: queryKeys.me,
    enabled: signedIn(),
    queryFn: async () => {
      const { data } = await api.GET('/v1/me');
      return data as MeView;
    },
  });
}

export function useOrganizations() {
  return useQuery({
    queryKey: queryKeys.orgs,
    enabled: signedIn(),
    queryFn: () =>
      fetchAllPages(async (cursor) => {
        const { data } = await api.GET('/v1/orgs', {
          params: { query: { limit: 100, ...(cursor ? { cursor } : {}) } },
        });
        return data as Page<OrganizationView>;
      }),
  });
}

export function useProjects(orgId: string | undefined) {
  return useQuery({
    queryKey: queryKeys.projects(orgId ?? ''),
    enabled: signedIn() && Boolean(orgId),
    queryFn: () =>
      fetchAllPages(async (cursor) => {
        const { data } = await api.GET('/v1/orgs/{org}/projects', {
          params: {
            path: { org: orgId as string },
            query: { limit: 100, ...(cursor ? { cursor } : {}) },
          },
        });
        return data as Page<ProjectView>;
      }),
  });
}

export function useDirectoryUsers(orgId: string | undefined) {
  return useQuery({
    queryKey: queryKeys.directoryUsers(orgId ?? ''),
    enabled: signedIn() && Boolean(orgId),
    queryFn: () =>
      fetchAllPages(async (cursor) => {
        const { data } = await api.GET('/v1/orgs/{org}/users', {
          params: {
            path: { org: orgId as string },
            query: { limit: 100, ...(cursor ? { cursor } : {}) },
          },
        });
        return data as Page<DirectoryUserView>;
      }),
  });
}

export function useDirectoryTeams(orgId: string | undefined) {
  return useQuery({
    queryKey: queryKeys.directoryTeams(orgId ?? ''),
    enabled: signedIn() && Boolean(orgId),
    queryFn: () =>
      fetchAllPages(async (cursor) => {
        const { data } = await api.GET('/v1/orgs/{org}/teams', {
          params: {
            path: { org: orgId as string },
            query: { limit: 100, ...(cursor ? { cursor } : {}) },
          },
        });
        return data as Page<TeamView>;
      }),
  });
}

export function useMyTasks(filters?: { kind?: 'approval' | 'human_task' }) {
  return useQuery({
    queryKey: queryKeys.myTasks(filters),
    enabled: signedIn(),
    queryFn: async () => {
      // The generated operation documents only the cursor parameter, so the
      // kind filter is applied client-side over the returned page.
      const { data } = await api.GET('/v1/me/tasks', {});
      const page = data as Schemas['InboxPage'];
      if (!filters?.kind) return page;
      return { ...page, items: page.items.filter((item) => item.kind === filters.kind) };
    },
  });
}

/** A project's whole runbook catalog (every page): `useRunbooks`, or one of `useQueries`. */
export function runbooksQueryOptions(projectId: string | undefined) {
  return {
    queryKey: queryKeys.runbooks(projectId ?? ''),
    enabled: signedIn() && Boolean(projectId),
    queryFn: () =>
      fetchAllPages(async (cursor) => {
        const { data } = await api.GET('/v1/projects/{p}/runbooks', {
          params: {
            path: { p: projectId as string },
            query: { limit: 100, ...(cursor ? { cursor } : {}) },
          },
        });
        return data as Page<Schemas['RunbookCatalogView']>;
      }),
  };
}

export function useRunbooks(projectId: string | undefined) {
  return useQuery(runbooksQueryOptions(projectId));
}

export function useExecution(executionId: string | undefined) {
  return useQuery({
    queryKey: queryKeys.execution(executionId ?? ''),
    enabled: signedIn() && Boolean(executionId),
    queryFn: async () => {
      const { data } = await api.GET('/v1/executions/{exe}', {
        params: { path: { exe: executionId as string } },
      });
      return data as Schemas['ExecutionView'];
    },
  });
}

export function useExecutionDocument(executionId: string | undefined) {
  return useQuery({
    queryKey: queryKeys.executionDocument(executionId ?? ''),
    enabled: signedIn() && Boolean(executionId),
    queryFn: async () => {
      const { data } = await api.GET('/v1/executions/{exe}/report', {
        params: { path: { exe: executionId as string } },
      });
      return data as Schemas['ExecutionDocumentView'];
    },
  });
}
