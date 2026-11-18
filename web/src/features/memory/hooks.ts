import {
  keepPreviousData,
  useInfiniteQuery,
  useMutation,
  useQueryClient,
} from '@tanstack/react-query';
import { api, ApiError, type Schemas } from '../../api/client';
import { shortId } from '../../ui/execution/names';

export type MemoryEntry = Schemas['MemoryEntryView'];
export type MemoryRevision = Schemas['MemoryRevisionView'];
export type MemoryStatus = Schemas['MemoryStatus'];
export type SaveMemoryRequest = Schemas['SaveMemoryRequest'];
export type MemoryPatchRequest = Schemas['MemoryPatchRequest'];
export type MemoryActorKind = Schemas['MemoryActorKind'];
export type MemorySource = Schemas['MemorySource'];
export type MemoryWriterView = Schemas['MemoryWriterView'];

export function memoryEtag(revision: number): string {
  return `"r${revision}"`;
}

export const MEMORY_ERRORS: Record<string, string> = {
  'memory.conflict': 'This entry changed since you opened it.',
  'memory.person_required': 'Only a person can take that action.',
  'memory.locked': 'This memory entry is locked against agent edits.',
  'memory.limit_reached': 'Memory limit reached for this runbook.',
  'memory.edit_limit': 'The daily edit limit has been reached for this entry.',
  'memory.invalid_transition': 'That status change is not allowed.',
  'memory.revision_not_found': 'That revision was not found or has been pruned.',
  'auth.scope_missing': 'Your token does not have the scope for that action.',
  'auth.forbidden': 'Your role in this project does not allow that action.',
  'memory.step_mismatch': 'The step does not match this entry.',
  'memory.invalid_step_id': 'That step ID is not valid for this runbook.',
  'memory.invalid_title': 'The title contains invalid characters.',
  'memory.invalid_tag': 'One or more tags contain invalid characters.',
};

export function memoryErrorText(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 412 || error.code === 'memory.conflict') {
      return 'This entry changed since you opened it.';
    }
    return MEMORY_ERRORS[error.code] ?? `The server refused that action (${error.code}).`;
  }
  return error instanceof Error ? error.message : 'That action failed.';
}

export function formatMemoryWriter(
  writer: MemoryWriterView | null | undefined,
  source: MemorySource | undefined,
  nameOf: (userId: string) => string,
): { label: string; kind: MemoryActorKind } {
  if (writer) {
    if (writer.kind === 'person') {
      const userId = writer.actor.startsWith('human:')
        ? writer.actor.slice('human:'.length)
        : writer.actor;
      return { label: nameOf(userId), kind: 'person' };
    }
    if (writer.kind === 'agent') {
      const parts = writer.actor.split(':');
      if (parts[2]) {
        return { label: `Agent (${nameOf(parts[2])})`, kind: 'agent' };
      }
      return { label: 'Agent', kind: 'agent' };
    }
    if (writer.kind === 'service') {
      return { label: 'Service', kind: 'service' };
    }
    if (writer.kind === 'execution') {
      const parts = writer.actor.split(':');
      const exe = parts[1] ? shortId(parts[1]) : 'run';
      return { label: `Run ${exe}`, kind: 'execution' };
    }
    return { label: writer.actor || 'System', kind: writer.kind };
  }

  if (source) {
    if (source.kind === 'person') {
      return { label: nameOf(source.user_id), kind: 'person' };
    }
    if (source.kind === 'agent') {
      return { label: `Agent (${nameOf(source.sponsor_user_id)})`, kind: 'agent' };
    }
    if (source.kind === 'service') {
      return { label: 'Service', kind: 'service' };
    }
    if (source.kind === 'step_run') {
      return { label: `Run ${shortId(source.execution_id)}`, kind: 'execution' };
    }
  }

  return { label: 'Unknown', kind: 'system' };
}

export const memoryKeys = {
  all: ['memory'] as const,
  runbook: (runbookId: string, filters?: { q?: string; status?: MemoryStatus; tags?: string[] }) =>
    ['runbooks', runbookId, 'memory', filters ?? {}] as const,
  project: (projectId: string, filters?: { q?: string; status?: MemoryStatus; tags?: string[] }) =>
    ['projects', projectId, 'memory', filters ?? {}] as const,
  entry: (memoryId: string) => ['memory', memoryId] as const,
  revisions: (memoryId: string) => ['memory', memoryId, 'revisions'] as const,
};

export function useRunbookMemory(
  runbookId: string | undefined,
  query: { q?: string; status?: MemoryStatus; tags?: string[] } = {},
) {
  const q = query.q?.trim() || undefined;
  const status = query.status;
  const tags = query.tags && query.tags.length > 0 ? query.tags.join(',') : undefined;

  return useInfiniteQuery({
    queryKey: memoryKeys.runbook(runbookId ?? '', { q, status, tags: query.tags }),
    enabled: Boolean(runbookId),
    initialPageParam: undefined as string | undefined,
    placeholderData: keepPreviousData,
    queryFn: async ({ pageParam, signal }) => {
      const { data } = await api.GET('/v1/runbooks/{rb}/memory', {
        signal,
        params: {
          path: { rb: runbookId as string },
          query: {
            limit: 50,
            ...(pageParam ? { cursor: pageParam } : {}),
            ...(q ? { q } : {}),
            ...(status ? { status } : {}),
            ...(tags ? { tags } : {}),
          },
        },
      });
      return data as Schemas['MemoryPage'];
    },
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
}

export function useProjectMemory(
  projectId: string | undefined,
  query: { q?: string; status?: MemoryStatus; tags?: string[] } = {},
) {
  const q = query.q?.trim() || undefined;
  const status = query.status;
  const tags = query.tags && query.tags.length > 0 ? query.tags.join(',') : undefined;

  return useInfiniteQuery({
    queryKey: memoryKeys.project(projectId ?? '', { q, status, tags: query.tags }),
    enabled: Boolean(projectId),
    initialPageParam: undefined as string | undefined,
    placeholderData: keepPreviousData,
    queryFn: async ({ pageParam, signal }) => {
      const { data, error } = await api.GET('/v1/projects/{p}/memory', {
        signal,
        params: {
          path: { p: projectId as string },
          query: {
            limit: 50,
            ...(pageParam ? { cursor: pageParam } : {}),
            ...(q ? { q } : {}),
            ...(status ? { status } : {}),
            ...(tags ? { tags } : {}),
          },
        },
      });
      if (error) throw error;
      return data as Schemas['MemoryPage'];
    },
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
}

export function useMemoryRevisions(memoryId: string | undefined) {
  return useInfiniteQuery({
    queryKey: memoryKeys.revisions(memoryId ?? ''),
    enabled: Boolean(memoryId),
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam, signal }) => {
      const { data } = await api.GET('/v1/memory/{mem}/revisions', {
        signal,
        params: {
          path: { mem: memoryId as string },
          query: {
            limit: 50,
            ...(pageParam ? { cursor: pageParam } : {}),
          },
        },
      });
      return data as Schemas['MemoryRevisionPage'];
    },
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
}

export type MemoryTarget = string | { runbookId?: string; projectId?: string } | undefined;

export function useCreateMemory(target: MemoryTarget) {
  const queryClient = useQueryClient();
  const runbookId = typeof target === 'string' ? target : target?.runbookId;
  const projectId = typeof target === 'object' ? target?.projectId : undefined;

  return useMutation({
    mutationFn: async (request: SaveMemoryRequest) => {
      if (runbookId) {
        const { data } = await api.POST('/v1/runbooks/{rb}/memory', {
          params: {
            path: { rb: runbookId },
            header: { 'Idempotency-Key': globalThis.crypto.randomUUID() },
          },
          body: request,
        });
        return data as MemoryEntry;
      }
      if (projectId) {
        const { data } = await api.POST('/v1/projects/{p}/memory', {
          params: {
            path: { p: projectId },
            header: { 'Idempotency-Key': globalThis.crypto.randomUUID() },
          },
          body: request,
        });
        return data as MemoryEntry;
      }
      throw new Error('Either runbookId or projectId must be provided.');
    },
    onSuccess: (created) => {
      if (runbookId) {
        void queryClient.invalidateQueries({
          queryKey: ['runbooks', runbookId, 'memory'],
        });
      }
      if (projectId) {
        void queryClient.invalidateQueries({
          queryKey: ['projects', projectId, 'memory'],
        });
      }
      return created;
    },
  });
}

export function usePatchMemory(target: MemoryTarget) {
  const queryClient = useQueryClient();
  const runbookId = typeof target === 'string' ? target : target?.runbookId;
  const projectId = typeof target === 'object' ? target?.projectId : undefined;

  return useMutation({
    mutationFn: async ({
      memoryId,
      revision,
      patch,
    }: {
      memoryId: string;
      revision: number;
      patch: MemoryPatchRequest;
    }) => {
      const { data } = await api.PATCH('/v1/memory/{mem}', {
        params: {
          path: { mem: memoryId },
          header: { 'If-Match': memoryEtag(revision) },
        },
        body: patch,
      });
      return data as MemoryEntry;
    },
    onSuccess: (updated, variables) => {
      if (runbookId) {
        void queryClient.invalidateQueries({
          queryKey: ['runbooks', runbookId, 'memory'],
        });
      }
      if (projectId) {
        void queryClient.invalidateQueries({
          queryKey: ['projects', projectId, 'memory'],
        });
      }
      void queryClient.invalidateQueries({
        queryKey: memoryKeys.revisions(variables.memoryId),
      });
      return updated;
    },
  });
}
