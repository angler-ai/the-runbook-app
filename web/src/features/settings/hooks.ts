import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, ApiError, type Schemas } from '../../api/client';
import { isCookieSession } from '../auth/session';

export type TokenView = Schemas['TokenView'];
export type TokenSecretView = Schemas['TokenSecretView'];
export type ConnectedAppView = Schemas['ConnectedAppView'];

export interface SessionItem {
  id: string;
  token: string;
  createdAt: string | number;
  expiresAt: string | number;
  ipAddress?: string | null;
  userAgent?: string | null;
}

export function friendlySettingsError(error: unknown): string {
  if (error instanceof ApiError) {
    return `The server refused that action (${error.code}).`;
  }
  return error instanceof Error ? error.message : 'Action failed.';
}

export function useSessions() {
  return useQuery({
    queryKey: ['auth', 'sessions'],
    enabled: isCookieSession(),
    queryFn: async () => {
      const res = await fetch('/api/auth/list-sessions', {
        headers: { 'x-runbook-csrf': '1' },
      });
      if (!res.ok) {
        throw new Error('Failed to load active sessions.');
      }
      return (await res.json()) as SessionItem[];
    },
  });
}

export function useRevokeSession() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (token: string) => {
      const res = await fetch('/api/auth/revoke-session', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          origin: globalThis.location?.origin ?? '',
          'x-runbook-csrf': '1',
        },
        body: JSON.stringify({ token }),
      });
      if (!res.ok) {
        throw new Error('Failed to revoke session.');
      }
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['auth', 'sessions'] });
    },
  });
}

export function useRevokeOtherSessions() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async () => {
      const res = await fetch('/api/auth/revoke-other-sessions', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          origin: globalThis.location?.origin ?? '',
          'x-runbook-csrf': '1',
        },
        body: JSON.stringify({}),
      });
      if (!res.ok) {
        throw new Error('Failed to revoke other sessions.');
      }
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['auth', 'sessions'] });
    },
  });
}

export function usePersonalTokens() {
  return useQuery({
    queryKey: ['tokens', 'personal'],
    enabled: isCookieSession(),
    queryFn: async () => {
      const { data } = await api.GET('/v1/tokens');
      const items = (data?.items ?? []) as TokenView[];
      return items.filter((t) => t.kind === 'personal');
    },
  });
}

export interface CreatePersonalTokenInput {
  name: string;
  scopes: string[];
  expires_at_ms: number;
  projects?: string[];
}

export function useCreatePersonalToken() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (input: CreatePersonalTokenInput): Promise<TokenSecretView> => {
      const { data } = await api.POST('/v1/tokens', {
        params: {
          header: {
            'Idempotency-Key': crypto.randomUUID(),
          },
        },
        body: {
          kind: 'personal',
          name: input.name,
          scopes: input.scopes,
          expires_at_ms: input.expires_at_ms,
          projects: input.projects && input.projects.length > 0 ? input.projects : null,
          actor_kind: null,
          role: null,
          user_id: null,
        },
      });
      return data as TokenSecretView;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['tokens'] });
    },
  });
}

export function useRevokeToken() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (id: string) => {
      await api.DELETE('/v1/tokens/{id}', {
        params: { path: { id } },
      });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['tokens'] });
    },
  });
}

export function useConnectedApps() {
  return useQuery({
    queryKey: ['me', 'apps'],
    queryFn: async () => {
      const { data } = await api.GET('/v1/me/apps');
      return (data?.items ?? []) as ConnectedAppView[];
    },
  });
}

export function useRevokeConnectedApp() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (client: string) => {
      await api.DELETE('/v1/me/apps/{client}', {
        params: { path: { client } },
      });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['me', 'apps'] });
    },
  });
}
