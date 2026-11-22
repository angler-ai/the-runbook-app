// Query hooks for multi-factor sign-in (J-3): the session's MFA state from
// the core, the person's factors from the auth service, and the org policy.
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { ACTION_HEADER, api, apiRuntime, newActionId, type ApiClient } from '../../api/client';
import { queryKeys } from '../../api/query-keys';
import { isCookieSession } from '../auth/session';
import {
  deletePasskey,
  disableTotp,
  listPasskeys,
  regenerateBackupCodes,
  registerPasskey,
  renamePasskey,
} from './authApi';

export interface MfaSecurityView {
  mfa_required: boolean;
  mfa_partial: boolean;
  mfa_enrolled: boolean;
  mfa_verified_at: number | null;
  mfa_method: 'passkey' | 'totp' | 'backup_code' | null;
  allow_passkeys: boolean;
  allow_authenticator: boolean;
  fresh_minutes: number;
  /**
   * The signed-in account's own factors (`GET /v1/me/security` only; the
   * partial `/v1/me` view and refusal bodies omit them). Null when the
   * sign-in service could not say.
   */
  authenticator_enrolled?: boolean | null;
  passkey_count?: number | null;
}

export type RequireMfa = 'off' | 'admins' | 'everyone';

export interface OrgSecurityPolicy {
  require_mfa: RequireMfa;
  allow_passkeys: boolean;
  allow_authenticator: boolean;
  fresh_minutes: number;
}

/** The `/v1/me` answer a partial session gets instead of MeView. */
export interface PartialMeView {
  user: { id: string; email: string };
  org: { id: string };
  security: MfaSecurityView;
}

export function isPartialMe(value: unknown): value is PartialMeView {
  if (!value || typeof value !== 'object') return false;
  const security = (value as { security?: unknown }).security;
  return (
    !!security &&
    typeof security === 'object' &&
    (security as { mfa_partial?: unknown }).mfa_partial === true
  );
}

export const mfaKeys = {
  security: ['me', 'security'] as const,
  passkeys: ['auth', 'passkeys'] as const,
  orgSecurity: (org: string) => ['orgs', org, 'security'] as const,
};

/** GET /v1/me/security on any client (the sign-in page uses a throwaway one). */
export async function fetchMySecurity(client: ApiClient = api): Promise<MfaSecurityView> {
  const { data } = await client.GET('/v1/me/security');
  return data as MfaSecurityView;
}

export function useMySecurity(enabled = true) {
  return useQuery({
    queryKey: mfaKeys.security,
    enabled: enabled && isCookieSession(),
    queryFn: () => fetchMySecurity(),
  });
}

export function usePasskeys(enabled = true) {
  return useQuery({
    queryKey: mfaKeys.passkeys,
    enabled: enabled && isCookieSession(),
    queryFn: listPasskeys,
  });
}

/**
 * Factor changes rotate the session cookie and change what the core says
 * about it: refresh everything that depends on the session's MFA state.
 */
export function useRefreshSecurity() {
  const queryClient = useQueryClient();
  return () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: mfaKeys.security }),
      queryClient.invalidateQueries({ queryKey: mfaKeys.passkeys }),
      queryClient.invalidateQueries({ queryKey: ['auth', 'sessions'] }),
      queryClient.invalidateQueries({ queryKey: queryKeys.me, exact: true }),
    ]);
}

export function useAddPasskey() {
  const refresh = useRefreshSecurity();
  return useMutation({ mutationFn: registerPasskey, onSuccess: refresh });
}

export function useRenamePasskey() {
  const refresh = useRefreshSecurity();
  return useMutation({
    mutationFn: ({ id, name }: { id: string; name: string }) => renamePasskey(id, name),
    onSuccess: refresh,
  });
}

export function useDeletePasskey() {
  const refresh = useRefreshSecurity();
  return useMutation({ mutationFn: deletePasskey, onSuccess: refresh });
}

export function useDisableTotp() {
  const refresh = useRefreshSecurity();
  return useMutation({ mutationFn: disableTotp, onSuccess: refresh });
}

export function useRegenerateBackupCodes(display: (codes: string[]) => void) {
  return useMutation({
    mutationFn: async () => {
      const result = await regenerateBackupCodes();
      display(result.backupCodes);
    },
  });
}

export function useOrgSecurity(orgId: string | undefined, enabled = true) {
  return useQuery({
    queryKey: mfaKeys.orgSecurity(orgId ?? ''),
    enabled: enabled && Boolean(orgId),
    queryFn: async () => {
      const { data } = await api.GET('/v1/orgs/{org}/security', {
        params: { path: { org: orgId as string } },
      });
      return data as OrgSecurityPolicy;
    },
  });
}

export function useUpdateOrgSecurity(orgId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (policy: OrgSecurityPolicy) => {
      const { data } = await api.PUT('/v1/orgs/{org}/security', {
        params: { path: { org: orgId as string } },
        body: policy,
      });
      return data as OrgSecurityPolicy;
    },
    onSuccess: (policy) => {
      queryClient.setQueryData(mfaKeys.orgSecurity(orgId ?? ''), policy);
      void queryClient.invalidateQueries({ queryKey: mfaKeys.security });
    },
  });
}

export interface PolicyMember {
  user_id: string;
  name: string;
  email: string | null;
}

export interface PolicyPreview {
  dry_run: true;
  policy: OrgSecurityPolicy;
  /** Members the policy would stop who cannot enroll an allowed factor themselves. */
  affected: (PolicyMember & { reason: 'factor_not_allowed' | 'cannot_enroll' })[];
}

/** `PUT /v1/orgs/{org}/security?dry_run=true`: who a policy would stop. Saves nothing. */
export function usePreviewOrgSecurity(orgId: string | undefined) {
  return useMutation({
    mutationFn: async (policy: OrgSecurityPolicy) => {
      const { data } = await api.PUT('/v1/orgs/{org}/security', {
        params: { path: { org: orgId as string }, query: { dry_run: true } },
        body: policy,
      });
      return data as PolicyPreview;
    },
  });
}

export function useResetMemberMfa(orgId: string | undefined) {
  return useMutation({
    mutationFn: async (userId: string) => {
      // One action, one idempotency key, kept across the step-up retry.
      const action = newActionId();
      try {
        await api.POST('/v1/orgs/{org}/users/{user}/mfa/reset', {
          params: { path: { org: orgId as string, user: userId } },
          headers: { [ACTION_HEADER]: action },
        });
      } finally {
        apiRuntime.forgetAction(action);
      }
    },
  });
}
