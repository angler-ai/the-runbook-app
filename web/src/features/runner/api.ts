import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, ApiError, type Schemas } from '../../api/client';

export type RunnerStatus = Schemas['RunnerStatusView'];
type Method = 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE';
const corePath = '/v1/runners/{ri}/proxy/{*path}';
export const runnerStatusKey = (org?: string) => ['runner-status', org] as const;
const runnerKey = (install?: string) => ['runner-proxy', install] as const;

export function useRunnerStatus(org?: string) {
  return useQuery({
    queryKey: runnerStatusKey(org),
    enabled: Boolean(org),
    refetchInterval: 30_000,
    retry: false,
    queryFn: async ({ signal }) => {
      const result = await api.GET('/v1/orgs/{org}/runner', {
        params: { path: { org: org! } },
        signal,
      });
      return result.data;
    },
  });
}

class VaultIdentityMissing extends Error {}

export async function runnerVaultIdentity(org: string, install: string) {
  const result = await api.GET('/v1/orgs/{org}/runner', {
    params: { path: { org } },
    cache: 'no-store',
  });
  if (result.data?.install_id !== install) throw new Error('Runner installation changed');
  if (!result.data.vault_identity_key) {
    throw new VaultIdentityMissing(
      'Ask an organization owner to register the vault identity before entering or rotating credentials.',
    );
  }
  return result.data.vault_identity_key;
}

export function proxyPath(install: string, path: string) {
  const segments = [install, ...path.split('?')[0]!.slice(1).split('/')];
  if (
    !path.startsWith('/') ||
    segments.some((s) => !/^[A-Za-z0-9_.:-]+$/.test(s) || s === '.' || s === '..')
  ) {
    throw new Error('Invalid runner route');
  }
  return `/v1/runners/${install}/proxy/runner/v1${path}`;
}

export async function runnerRequest<T>(
  install: string,
  path: string,
  method: Method = 'GET',
  body?: unknown,
  signal?: AbortSignal,
): Promise<T> {
  const url = proxyPath(install, path);
  const options = {
    params: { header: { 'X-Runbook-CSRF': '1' as const }, path: { ri: install, path } },
    pathSerializer: (template: string) => template.replace(corePath, url),
    signal,
    parseAs: 'text' as const,
  };
  const result =
    method === 'GET' || method === 'DELETE'
      ? await api.request(method === 'GET' ? 'get' : 'delete', corePath, options)
      : await api.request(method.toLowerCase() as 'post' | 'patch' | 'put', corePath, {
          ...options,
          body: body as never,
        });
  const data: unknown = result.data;
  const value = typeof data === 'string' ? data : '';
  const contentType = result.response.headers.get('content-type') ?? '';
  return (
    value && !contentType.startsWith('text/event-stream') ? JSON.parse(value) : value || undefined
  ) as T;
}

export function runnerQueryOptions<T>(
  status: RunnerStatus | undefined,
  path: string,
  enabled = true,
) {
  const install = status?.install_id ?? undefined;
  return {
    queryKey: [...runnerKey(install), path],
    enabled: Boolean(install && status?.status === 'online' && enabled),
    queryFn: ({ signal }: { signal: AbortSignal }) =>
      runnerRequest<T>(install!, path, 'GET', undefined, signal),
    retry: false as const,
  };
}

export function useRunnerQuery<T>(status: RunnerStatus | undefined, path: string, enabled = true) {
  return useQuery(runnerQueryOptions<T>(status, path, enabled));
}

export function useRunnerMutation(status: RunnerStatus | undefined) {
  const queryClient = useQueryClient();
  const install = status?.install_id ?? undefined;
  return useMutation({
    mutationFn: (action: { path: string; method: Method; body?: unknown }) => {
      if (!install || status?.status !== 'online') throw new Error('Runner is offline');
      return runnerRequest<unknown>(install, action.path, action.method, action.body);
    },
    onSuccess: () => queryClient.invalidateQueries({ queryKey: runnerKey(install) }),
  });
}

export function runnerError(error: unknown) {
  if (error instanceof VaultIdentityMissing) return error.message;
  if (error instanceof ApiError) {
    const reason = error.body.error as { code?: string; message?: string } | undefined;
    const code = reason?.code ?? error.code;
    if (code === 'runner.oauth_clock_skew')
      return 'The vault’s signed authorization is outside the accepted time window. Check the vault and Runbook clocks.';
    if (code.startsWith('oauth.') && reason?.message) return reason.message;
    if (error.status === 403) return 'You do not have access to this runner operation.';
    if (error.status === 404) return 'This runner resource is unavailable.';
    if (error.status === 503 && code === 'runner.offline')
      return 'Runner is unavailable. Try again when it is online.';
    if (error.status === 409) {
      if (code === 'credential.revoked')
        return 'The credential is revoked. Re-create it to use it again.';
      return reason?.message ?? 'This resource changed. Refresh and try again.';
    }
    if (error.status < 500) {
      if (error.errors.length)
        return error.errors.map(({ path, message }) => `${path}: ${message}`).join(' ');
      return reason?.message ?? 'The runner rejected this request.';
    }
    return `The runner could not complete this operation.${error.requestId ? ` Request ID: ${error.requestId}.` : ''}`;
  }
  if (error instanceof TypeError)
    return 'Could not reach the runner. Check your connection and try again.';
  return error instanceof Error ? error.message : 'The runner returned an unexpected response.';
}

export type Items<T> = { items: T[] };
export type Row = Record<string, unknown>;
