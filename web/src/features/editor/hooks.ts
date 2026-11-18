// Draft reads and writes (M5). The draft's ETag travels with its value, so
// every save and publish names the exact revision it was made against
// (If-Match). Publish and create are idempotent user actions (D20): one key
// per action, reused when the same action is retried after a network error
// or a 5xx, and dropped once the server has answered definitively.
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { ACTION_HEADER, api, ApiError, apiRuntime, type Schemas } from '../../api/client';
import { queryKeys } from '../../api/query-keys';
import type { Diagnostic } from '../../wasm/runbook';

export type DraftView = Schemas['DraftView'];
export type DraftWriteView = Schemas['DraftWriteView'];
export type PublishedVersion = Schemas['PublishedVersionView'];
export type RunbookView = Schemas['RunbookCatalogView'];

export interface LoadedDraft {
  draft: DraftView;
  /** The strong draft ETag (`"draft-<rb>-<revision>"`); null only if a proxy dropped it. */
  etag: string | null;
}

export interface SavedDraft {
  write: DraftWriteView;
  etag: string | null;
}

/** A definitive answer: retrying the same request cannot succeed, so the action is over. */
export function isDefinitive(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    error.status >= 400 &&
    error.status < 500 &&
    error.status !== 408 &&
    error.status !== 429
  );
}

/**
 * Publication refusals carry compiler diagnostics in the problem's `errors`
 * (422; `path`, `code`, `message`, and `line`/`column` when known).
 */
export function problemDiagnostics(error: ApiError): Diagnostic[] {
  return error.errors.map((e) => {
    const extra = e as Partial<Diagnostic>;
    return {
      path: e.path,
      code: e.code,
      message: e.message,
      line: typeof extra.line === 'number' ? extra.line : null,
      column: typeof extra.column === 'number' ? extra.column : null,
    };
  });
}

export function draftQueryOptions(runbookId: string) {
  return {
    queryKey: queryKeys.runbookDraft(runbookId),
    queryFn: async (): Promise<LoadedDraft> => {
      const { data, response } = await api.GET('/v1/runbooks/{rb}/draft', {
        params: { path: { rb: runbookId } },
        // The ETag decides what a save overwrites: never take it from the HTTP cache.
        cache: 'no-store',
      });
      return { draft: data as DraftView, etag: response.headers.get('etag') };
    },
    // The editor seeds its text once; background refetches would only move the ETag
    // under unsaved edits. A 412 or an explicit reload fetches it again.
    staleTime: Infinity,
    refetchOnWindowFocus: false,
  } as const;
}

export function useDraft(runbookId: string) {
  return useQuery(draftQueryOptions(runbookId));
}

export function runbookQueryOptions(runbookId: string) {
  return {
    queryKey: queryKeys.runbook(runbookId),
    queryFn: async (): Promise<RunbookView> => {
      const { data } = await api.GET('/v1/runbooks/{rb}', {
        params: { path: { rb: runbookId } },
      });
      return data as RunbookView;
    },
  } as const;
}

export function useRunbook(runbookId: string) {
  return useQuery(runbookQueryOptions(runbookId));
}

/** Save `source` as the draft of `runbookId` over the revision `etag` names (If-Match). */
export async function patchDraft(
  runbookId: string,
  source: string,
  etag: string,
): Promise<SavedDraft> {
  const { data, response } = await api.PATCH('/v1/runbooks/{rb}/draft', {
    params: { path: { rb: runbookId }, header: { 'If-Match': etag } },
    body: { source },
  });
  return { write: data as DraftWriteView, etag: response.headers.get('etag') };
}

export function useSaveDraft(runbookId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ source, etag }: { source: string; etag: string }) =>
      patchDraft(runbookId, source, etag),
    onSuccess: ({ write, etag }) => {
      queryClient.setQueryData<LoadedDraft>(queryKeys.runbookDraft(runbookId), {
        draft: { runbook_id: write.runbook_id, source: write.source, revision: write.revision },
        etag,
      });
      // The catalog title follows the draft's metadata.title.
      void queryClient.invalidateQueries({ queryKey: queryKeys.runbook(runbookId), exact: true });
    },
  });
}

export function usePublish(runbookId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async ({
      etag,
      actionId,
    }: {
      etag: string;
      actionId: string;
    }): Promise<PublishedVersion> => {
      try {
        const { data } = await api.POST('/v1/runbooks/{rb}/versions', {
          params: {
            path: { rb: runbookId },
            header: {
              'If-Match': etag,
              'Idempotency-Key': apiRuntime.idempotencyKeyFor(actionId),
              [ACTION_HEADER]: actionId,
            },
          },
        });
        apiRuntime.forgetAction(actionId);
        return data as PublishedVersion;
      } catch (error) {
        if (isDefinitive(error)) apiRuntime.forgetAction(actionId);
        throw error;
      }
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.runbook(runbookId), exact: true });
      void queryClient.invalidateQueries({ queryKey: queryKeys.runbookVersions(runbookId) });
    },
  });
}

export function useCreateRunbook(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async ({
      source,
      slug,
      actionId,
    }: {
      source: string;
      slug: string;
      actionId: string;
    }): Promise<SavedDraft> => {
      try {
        const { data, response } = await api.POST('/v1/projects/{p}/runbooks', {
          params: {
            path: { p: projectId },
            header: {
              'Idempotency-Key': apiRuntime.idempotencyKeyFor(actionId),
              [ACTION_HEADER]: actionId,
            },
          },
          body: { source, slug: slug.trim() === '' ? null : slug.trim() },
        });
        apiRuntime.forgetAction(actionId);
        return { write: data as DraftWriteView, etag: response.headers.get('etag') };
      } catch (error) {
        if (isDefinitive(error)) apiRuntime.forgetAction(actionId);
        throw error;
      }
    },
    onSuccess: ({ write, etag }) => {
      // The new runbook's editor opens on this exact revision without a refetch.
      queryClient.setQueryData<LoadedDraft>(queryKeys.runbookDraft(write.runbook_id), {
        draft: { runbook_id: write.runbook_id, source: write.source, revision: write.revision },
        etag,
      });
      void queryClient.invalidateQueries({ queryKey: queryKeys.runbooks(projectId) });
    },
  });
}

/** What the signed-in credential may do in the editor (the server stays the authority). */
export interface EditorPermissions {
  canWrite: boolean;
  canPublish: boolean;
}

export function editorPermissions(scopes: readonly string[] | undefined): EditorPermissions {
  const has = (scope: string) => scopes?.includes(scope) ?? false;
  return { canWrite: has('runbooks:write'), canPublish: has('runbooks:publish') };
}
