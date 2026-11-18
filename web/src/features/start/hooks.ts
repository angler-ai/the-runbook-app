// Start-run mutations and the version form read (B1.6).
import { useMutation, useQuery } from '@tanstack/react-query';
import { ACTION_HEADER, api, apiRuntime, type Schemas } from '../../api/client';
import { queryKeys } from '../../api/query-keys';

export type VersionForm = Schemas['VersionFormView'];

export function useVersionForm(runbookId: string | undefined, version: string) {
  return useQuery({
    queryKey: queryKeys.versionForm(runbookId ?? '', version),
    enabled: Boolean(runbookId),
    queryFn: async () => {
      const { data } = await api.GET('/v1/runbooks/{rb}/versions/{version}/form', {
        params: { path: { rb: runbookId as string, version } },
      });
      return data as VersionForm;
    },
  });
}

export interface StartInput {
  runbook: string;
  version?: number | string;
  title?: string;
  tags: string[];
  inputs: Record<string, unknown>;
  fields: Record<string, unknown>;
  actionId: string;
}

/** One idempotency key per dialog submission; retries reuse it. */
export function useStartRun(projectId: string) {
  return useMutation({
    mutationFn: async (input: StartInput) => {
      const { data } = await api.POST('/v1/projects/{p}/executions', {
        params: {
          path: { p: projectId },
          header: {
            'Idempotency-Key': apiRuntime.idempotencyKeyFor(input.actionId),
            [ACTION_HEADER]: input.actionId,
          },
        },
        body: {
          runbook: input.runbook,
          ...(input.version !== undefined ? { version: input.version } : {}),
          title: input.title?.trim() ? input.title.trim() : null,
          tags: input.tags,
          inputs: input.inputs,
          fields: input.fields,
        },
      });
      apiRuntime.forgetAction(input.actionId);
      return data as Schemas['ExecutionView'];
    },
  });
}
