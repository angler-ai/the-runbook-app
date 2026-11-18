import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, ApiError, type Schemas } from '../../api/client';

export type WebhookView = Schemas['WebhookView'];
export type WebhookPage = Schemas['WebhookPage'];
export type WebhookSecretView = Schemas['WebhookSecretView'];
export type CreateWebhookRequest = Schemas['CreateWebhookRequest'];
export type UpdateWebhookRequest = Schemas['UpdateWebhookRequest'];
export type DeliveryView = Schemas['DeliveryView'];
export type DeliveryPage = Schemas['DeliveryPage'];

export const projectWebhooksKey = (projectId: string | undefined) =>
  ['projects', projectId ?? '', 'webhooks'] as const;

export const webhookDeliveriesKey = (projectId: string | undefined, webhookId: string | undefined) =>
  ['projects', projectId ?? '', 'webhooks', webhookId ?? '', 'deliveries'] as const;

export function friendlyWebhookError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 403) {
      return 'You do not have permission to manage webhooks. Project administrator role is required.';
    }
    if (error.code === 'webhooks.limit_reached') {
      return 'Project limit of 20 webhooks reached. Delete an existing webhook to add a new one.';
    }
    if (error.code === 'webhooks.url_invalid' || error.code === 'webhooks.url_https_required') {
      return 'The webhook URL must be a valid HTTPS URL without credentials.';
    }
    if (error.code === 'webhooks.target_forbidden') {
      return 'Webhook delivery to internal or private IP addresses is not permitted.';
    }
    if (error.code === 'webhooks.conflict') {
      return 'A webhook with that name or configuration already exists.';
    }
    return error.message || error.code;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return 'An unexpected error occurred.';
}

export function useWebhooks(projectId: string | undefined, options?: { enabled?: boolean }) {
  return useQuery({
    queryKey: projectWebhooksKey(projectId),
    enabled: Boolean(projectId) && (options?.enabled ?? true),
    queryFn: async () => {
      const { data } = await api.GET('/v1/projects/{p}/webhooks', {
        params: { path: { p: projectId ?? '' } },
      });
      return (data as WebhookPage) ?? { items: [] };
    },
  });
}

export function useCreateWebhook(projectId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (body: CreateWebhookRequest) => {
      const key = `create_webhook_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
      const { data } = await api.POST('/v1/projects/{p}/webhooks', {
        params: {
          path: { p: projectId ?? '' },
          header: { 'Idempotency-Key': key },
        },
        body,
      });
      return data as WebhookSecretView;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: projectWebhooksKey(projectId) });
    },
  });
}

export function useUpdateWebhook(projectId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async ({ id, body }: { id: string; body: UpdateWebhookRequest }) => {
      const { data } = await api.PATCH('/v1/projects/{p}/webhooks/{id}', {
        params: { path: { p: projectId ?? '', id } },
        body,
      });
      return data as WebhookView;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: projectWebhooksKey(projectId) });
    },
  });
}

export function useDeleteWebhook(projectId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (id: string) => {
      await api.DELETE('/v1/projects/{p}/webhooks/{id}', {
        params: { path: { p: projectId ?? '', id } },
      });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: projectWebhooksKey(projectId) });
    },
  });
}

export function useRotateWebhookSecret(projectId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (id: string) => {
      const key = `rotate_secret_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
      const { data } = await api.POST('/v1/projects/{p}/webhooks/{id}/rotate-secret', {
        params: {
          path: { p: projectId ?? '', id },
          header: { 'Idempotency-Key': key },
        },
      });
      return data as WebhookSecretView;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: projectWebhooksKey(projectId) });
    },
  });
}

export function useTestWebhook(projectId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (id: string) => {
      const key = `test_webhook_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
      const { data } = await api.POST('/v1/projects/{p}/webhooks/{id}/test', {
        params: {
          path: { p: projectId ?? '', id },
          header: { 'Idempotency-Key': key },
        },
      });
      return data as DeliveryView;
    },
    onSuccess: (_data, id) => {
      void queryClient.invalidateQueries({ queryKey: webhookDeliveriesKey(projectId, id) });
    },
  });
}

export function useWebhookDeliveries(
  projectId: string | undefined,
  webhookId: string | undefined,
  options?: { enabled?: boolean },
) {
  return useInfiniteQuery({
    queryKey: webhookDeliveriesKey(projectId, webhookId),
    enabled: Boolean(projectId && webhookId) && (options?.enabled ?? true),
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam }) => {
      const { data } = await api.GET('/v1/projects/{p}/webhooks/{id}/deliveries', {
        params: {
          path: { p: projectId ?? '', id: webhookId ?? '' },
          query: { limit: 20, ...(pageParam ? { cursor: pageParam } : {}) },
        },
      });
      return (data as DeliveryPage) ?? { items: [], next_cursor: null };
    },
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
  });
}

export function useRedeliver(projectId: string | undefined, webhookId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (deliveryId: string) => {
      const key = `redeliver_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
      const { data } = await api.POST(
        '/v1/projects/{p}/webhooks/{id}/deliveries/{d}/redeliver',
        {
          params: {
            path: { p: projectId ?? '', id: webhookId ?? '', d: deliveryId },
            header: { 'Idempotency-Key': key },
          },
        },
      );
      return data as DeliveryView;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: webhookDeliveriesKey(projectId, webhookId) });
    },
  });
}
