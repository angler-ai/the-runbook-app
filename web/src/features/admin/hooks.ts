import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, ApiError } from '../../api/client';
import { fetchAllPages, signedIn } from '../../api/hooks';
import type { components } from '../../api/schema';

interface Page<Item> {
  items: Item[];
  next_cursor?: string | null;
}

export type DirectoryUserView = components['schemas']['DirectoryUserView'];
export type OrganizationRole = components['schemas']['OrganizationRole'];
export type ProjectRole = components['schemas']['ProjectRole'];
export type InvitationView = components['schemas']['InvitationView'];
export type InvitationProjectGrant = components['schemas']['InvitationProjectGrant'];
export type TeamView = components['schemas']['TeamView'];
export type TeamMemberItem = { user_id: string };
export type ProjectMemberItem = { user_id: string; role: ProjectRole };
export type TokenView = components['schemas']['TokenView'];
export type TokenSecretView = components['schemas']['TokenSecretView'];

export function friendlyAdminError(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.status === 403)
      return 'You do not have permission to perform this administration action.';
    if (err.status === 404) return 'The requested resource was not found.';
    if (err.status === 409) return 'That item already exists or a conflict occurred.';
    if (err.code === 'tokens.exceeds_caller')
      return 'The requested role or scopes exceed your own permissions.';
    return `The server refused that action (${err.code}).`;
  }
  return 'An unexpected error occurred. Please try again.';
}

export function useOrganizationUsers(orgId: string | undefined, options?: { enabled?: boolean }) {
  return useQuery({
    queryKey: ['orgs', orgId ?? '', 'users'],
    enabled: signedIn() && Boolean(orgId) && (options?.enabled ?? true),
    queryFn: () =>
      fetchAllPages<DirectoryUserView>(async (cursor) => {
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

export function useUpdateOrganizationUser(orgId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async ({ userId, active }: { userId: string; active: boolean }) => {
      const { data } = await api.PATCH('/v1/orgs/{org}/users/{user}', {
        params: {
          path: { org: orgId as string, user: userId },
        },
        body: { active },
      });
      return data;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['orgs', orgId ?? '', 'users'] });
      void queryClient.invalidateQueries({ queryKey: ['directory', 'users', orgId ?? ''] });
    },
  });
}

export function useInvitations(orgId: string | undefined, options?: { enabled?: boolean }) {
  return useQuery({
    queryKey: ['orgs', orgId ?? '', 'invitations'],
    enabled: signedIn() && Boolean(orgId) && (options?.enabled ?? true),
    queryFn: () =>
      fetchAllPages<InvitationView>(async (cursor) => {
        const { data } = await api.GET('/v1/orgs/{org}/invitations', {
          params: {
            path: { org: orgId as string },
            query: { limit: 100, ...(cursor ? { cursor } : {}) },
          },
        });
        return data as Page<InvitationView>;
      }),
  });
}

export interface CreateInvitationInput {
  email: string;
  /** The invitee's name, for the account they get. */
  name?: string;
  role: OrganizationRole;
  projects?: InvitationProjectGrant[];
}

export function useCreateInvitation(orgId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (input: CreateInvitationInput) => {
      const { data } = await api.POST('/v1/orgs/{org}/invitations', {
        params: {
          path: { org: orgId as string },
          header: {
            'Idempotency-Key': crypto.randomUUID(),
          },
        },
        body: {
          email: input.email.trim(),
          ...(input.name?.trim() ? { name: input.name.trim() } : {}),
          role: input.role,
          projects: input.projects && input.projects.length > 0 ? input.projects : null,
        },
      });
      return data;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['orgs', orgId ?? '', 'invitations'] });
    },
  });
}

export function useRevokeInvitation(orgId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (invitationId: string) => {
      await api.DELETE('/v1/orgs/{org}/invitations/{inv}', {
        params: {
          path: { org: orgId as string, inv: invitationId },
        },
      });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['orgs', orgId ?? '', 'invitations'] });
    },
  });
}

export function useTeams(orgId: string | undefined, options?: { enabled?: boolean }) {
  return useQuery({
    queryKey: ['orgs', orgId ?? '', 'teams'],
    enabled: signedIn() && Boolean(orgId) && (options?.enabled ?? true),
    queryFn: () =>
      fetchAllPages<TeamView>(async (cursor) => {
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

export function useCreateTeam(orgId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async ({ title, slug }: { title: string; slug: string }) => {
      const { data } = await api.POST('/v1/orgs/{org}/teams', {
        params: {
          path: { org: orgId as string },
          header: {
            'Idempotency-Key': crypto.randomUUID(),
          },
        },
        body: {
          title: title.trim(),
          slug: slug.trim(),
        },
      });
      return data;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['orgs', orgId ?? '', 'teams'] });
      void queryClient.invalidateQueries({ queryKey: ['directory', 'teams', orgId ?? ''] });
    },
  });
}

export function useTeamMembers(orgId: string | undefined, teamId: string | undefined) {
  return useQuery({
    queryKey: ['orgs', orgId ?? '', 'teams', teamId ?? '', 'members'],
    enabled: signedIn() && Boolean(orgId && teamId),
    queryFn: () =>
      fetchAllPages<TeamMemberItem>(async (cursor) => {
        const { data } = await api.GET('/v1/orgs/{org}/teams/{team}/members', {
          params: {
            path: { org: orgId as string, team: teamId as string },
            query: { limit: 100, ...(cursor ? { cursor } : {}) },
          },
        });
        return data as unknown as Page<TeamMemberItem>;
      }),
  });
}

export function useAddTeamMember(orgId: string | undefined, teamId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (userId: string) => {
      const { data } = await api.POST('/v1/orgs/{org}/teams/{team}/members', {
        params: {
          path: { org: orgId as string, team: teamId as string },
          header: {
            'Idempotency-Key': crypto.randomUUID(),
          },
        },
        body: { user_id: userId },
      });
      return data;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: ['orgs', orgId ?? '', 'teams', teamId ?? '', 'members'],
      });
      void queryClient.invalidateQueries({ queryKey: ['directory', 'teams', orgId ?? ''] });
    },
  });
}

export function useRemoveTeamMember(orgId: string | undefined, teamId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (userId: string) => {
      await api.DELETE('/v1/orgs/{org}/teams/{team}/members/{user}', {
        params: {
          path: { org: orgId as string, team: teamId as string, user: userId },
        },
      });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: ['orgs', orgId ?? '', 'teams', teamId ?? '', 'members'],
      });
      void queryClient.invalidateQueries({ queryKey: ['directory', 'teams', orgId ?? ''] });
    },
  });
}

export function useProjectMembers(projectId: string | undefined, options?: { enabled?: boolean }) {
  return useQuery({
    queryKey: ['projects', projectId ?? '', 'members'],
    enabled: signedIn() && Boolean(projectId) && (options?.enabled ?? true),
    queryFn: () =>
      fetchAllPages<ProjectMemberItem>(async (cursor) => {
        const { data } = await api.GET('/v1/projects/{p}/members', {
          params: {
            path: { p: projectId as string },
            query: { limit: 100, ...(cursor ? { cursor } : {}) },
          },
        });
        return data as unknown as Page<ProjectMemberItem>;
      }),
  });
}

export function useGrantProjectMember(projectId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async ({ userId, role }: { userId: string; role: ProjectRole }) => {
      const { data } = await api.POST('/v1/projects/{p}/members', {
        params: {
          path: { p: projectId as string },
          header: {
            'Idempotency-Key': crypto.randomUUID(),
          },
        },
        body: { user_id: userId, role },
      });
      return data;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['projects', projectId ?? '', 'members'] });
    },
  });
}

export function useRevokeProjectMember(projectId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (userId: string) => {
      await api.DELETE('/v1/projects/{p}/members/{user}', {
        params: {
          path: { p: projectId as string, user: userId },
        },
      });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['projects', projectId ?? '', 'members'] });
    },
  });
}

export function useCreateProject(orgId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async ({ title, slug }: { title: string; slug: string }) => {
      const { data } = await api.POST('/v1/orgs/{org}/projects', {
        params: {
          path: { org: orgId as string },
          header: {
            'Idempotency-Key': crypto.randomUUID(),
          },
        },
        body: {
          title: title.trim(),
          slug: slug.trim(),
        },
      });
      return data;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['projects', orgId ?? ''] });
    },
  });
}

export function useServiceTokens(options?: { enabled?: boolean }) {
  return useQuery({
    queryKey: ['tokens', 'service'],
    enabled: signedIn() && (options?.enabled ?? true),
    queryFn: async (): Promise<TokenView[]> => {
      const { data } = await api.GET('/v1/tokens', {});
      const allTokens = (data?.items ?? []) as TokenView[];
      return allTokens.filter((t) => t.kind === 'service');
    },
  });
}

export interface CreateServiceTokenInput {
  name: string;
  project: string;
  role: ProjectRole;
  scopes: string[];
  expires_at_ms: number | null;
}

export function useCreateServiceToken() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (input: CreateServiceTokenInput): Promise<TokenSecretView> => {
      const { data } = await api.POST('/v1/tokens', {
        params: {
          header: {
            'Idempotency-Key': crypto.randomUUID(),
          },
        },
        body: {
          kind: 'service',
          name: input.name.trim(),
          projects: [input.project],
          role: input.role,
          scopes: input.scopes,
          expires_at_ms: input.expires_at_ms,
          actor_kind: null,
          user_id: null,
        },
      });
      return data as TokenSecretView;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['tokens', 'service'] });
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
      void queryClient.invalidateQueries({ queryKey: ['tokens', 'service'] });
      void queryClient.invalidateQueries({ queryKey: ['tokens'] });
    },
  });
}
