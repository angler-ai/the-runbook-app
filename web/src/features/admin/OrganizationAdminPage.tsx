import { Link } from '@tanstack/react-router';
import { useRunnerStatus } from '../runner/api';
import { SetupCard } from '../runner/presentation';
import { useMemo, useState } from 'react';
import { useSearch } from '@tanstack/react-router';
import { PageHeader } from '../../ui/layout/PageHeader';
import { Button } from '../../ui/primitives/button';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../../ui/primitives/tabs';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '../../ui/primitives/select';
import { useMe } from '../../api/hooks';
import { useOrgContext } from '../context/OrgContext';
import { useDirectoryNames } from '../context/names';
import { formatRelativeTime } from '../../lib/format';
import {
  useAddTeamMember,
  useInvitations,
  useOrganizationUsers,
  useRemoveTeamMember,
  useRevokeInvitation,
  useRevokeToken,
  useServiceTokens,
  useTeamMembers,
  useTeams,
  useUpdateOrganizationUser,
  type DirectoryUserView,
  type TeamView,
} from './hooks';
import { InviteMemberDialog } from './InviteMemberDialog';
import { CreateTeamDialog } from './CreateTeamDialog';
import { CreateServiceTokenDialog } from './CreateServiceTokenDialog';
import { FolderKanban, Key, Lock, Mail, Plus, Shield, UserPlus, Users, X } from 'lucide-react';
import { OrgProjectsTab } from './OrgProjectsTab';
import { OrgSecurityTab } from '../mfa/OrgSecurityTab';
import { useMySecurity, useResetMemberMfa } from '../mfa/hooks';
import { TokenReplacementBadge } from '../settings/TokenReplacementBadge';
import { OPERATOR_RESET_HINT, securityCodeMessage } from '../mfa/messages';
import { ConfirmDialog } from '../../ui/actions/ConfirmDialog';
import { toast } from 'sonner';
import { ApiError } from '../../api/client';

export function OrganizationAdminPage() {
  const me = useMe();
  const { org } = useOrgContext();
  const { nameOf } = useDirectoryNames();

  let initialTab = 'members';
  try {
    const search = useSearch({ strict: false }) as { tab?: string };
    if (
      search?.tab &&
      ['members', 'projects', 'teams', 'tokens', 'security'].includes(search.tab)
    ) {
      initialTab = search.tab;
    }
  } catch {
    // Router context fallback
  }

  const [tab, setTab] = useState(initialTab);
  const [inviteOpen, setInviteOpen] = useState(false);
  const [createTeamOpen, setCreateTeamOpen] = useState(false);
  const [createTokenOpen, setCreateTokenOpen] = useState(false);
  const [resetTarget, setResetTarget] = useState<DirectoryUserView | null>(null);

  const orgId = org?.id;
  const runner = useRunnerStatus(me.data?.org.id);
  const isOwnerOrAdmin = me.data?.org.role === 'owner' || me.data?.org.role === 'admin';
  const isOwner = me.data?.org.role === 'owner';
  const resetMfaMutation = useResetMemberMfa(orgId);

  async function resetMfa(user: DirectoryUserView) {
    try {
      await resetMfaMutation.mutateAsync(user.id);
      toast.success(
        `${user.name}'s second factors were reset. They set up a new one next time they sign in.`,
      );
    } catch (err) {
      const code = err instanceof ApiError ? err.code : null;
      toast.error(
        code === 'auth.mfa_step_up'
          ? 'Confirm it is you with a second factor, then reset again.'
          : code === 'resource.not_found'
            ? `${user.name} has no sign-in identity to reset.`
            : code === 'auth.mfa_operator_reset_required'
              ? `${user.name} also belongs to another organization or shares a sign-in, so an owner cannot reset them. ${OPERATOR_RESET_HINT}`
              : ((code && securityCodeMessage(code)) ??
                'The second factors could not be reset. Try again.'),
      );
    }
  }
  const hasProjectAdminScope = Boolean(me.data?.credential.scopes?.includes('project:admin'));
  const hasTokensManageScope = Boolean(me.data?.credential.scopes?.includes('tokens:manage'));

  // Data queries
  const usersQuery = useOrganizationUsers(orgId, { enabled: hasProjectAdminScope });
  const invitationsQuery = useInvitations(orgId, { enabled: hasProjectAdminScope });
  const teamsQuery = useTeams(orgId, { enabled: hasProjectAdminScope });
  const serviceTokensQuery = useServiceTokens({ enabled: hasTokensManageScope });

  // Mutations
  const updateUserMutation = useUpdateOrganizationUser(orgId);
  const revokeInvitationMutation = useRevokeInvitation(orgId);
  const revokeTokenMutation = useRevokeToken();

  const users = usersQuery.data ?? [];
  const invitations = (invitationsQuery.data ?? []).filter((i) => i.state === 'pending');
  const teams = teamsQuery.data ?? [];
  const serviceTokens = serviceTokensQuery.data ?? [];
  // A service token needs issuance proof whenever the organization requires
  // MFA for any role. Token managers are owners or admins, whom every
  // required setting covers, so their own requirement says whether it is on.
  const serviceTokensNeedProof = useMySecurity(hasTokensManageScope).data?.mfa_required === true;

  if (me.isLoading) {
    return (
      <div className="w-full">
        <PageHeader
          title="Organization administration"
          description={`Organization settings, access, and credentials for ${org?.title ?? 'the organization'}.`}
        />
        <div className="mt-8 flex justify-center py-12 text-sm text-fg-muted">
          Loading organization details…
        </div>
      </div>
    );
  }

  if (!isOwnerOrAdmin) {
    return (
      <div className="w-full">
        <PageHeader
          title="Organization administration"
          description="Manage members, teams, and service tokens."
        />
        <div
          role="region"
          aria-label="Access restricted"
          className="rounded-lg border border-border bg-surface p-8 text-center"
        >
          <Shield className="mx-auto size-10 text-fg-muted" aria-hidden="true" />
          <h2 className="mt-3 text-base font-semibold text-fg">Ask an organization admin</h2>
          <p className="mt-1 text-sm text-fg-muted">
            Managing organization members, invitations, teams, and service tokens requires an
            organization owner or administrator role.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="w-full">
      <PageHeader
        title="Organization administration"
        description={`Organization settings, access, and credentials for ${org?.title ?? 'the organization'}.`}
      />

      {runner.data && (
        <div className="mb-6">
          {runner.data.status === 'none' ? (
            <SetupCard admin={isOwnerOrAdmin} />
          ) : (
            <Link to="/runner/overview" className="text-accent underline">
              Runner
            </Link>
          )}
        </div>
      )}
      <Tabs value={tab} onValueChange={setTab} className="gap-6">
        <TabsList className="mb-6">
          <TabsTrigger value="members" className="gap-2">
            <Users className="size-4" />
            <span>Members & Invitations</span>
            <span className="rounded bg-surface-raised px-1.5 py-0.5 text-2xs font-mono text-fg-muted">
              {users.length}
            </span>
          </TabsTrigger>
          <TabsTrigger value="projects" className="gap-2">
            <FolderKanban className="size-4" />
            <span>Projects</span>
          </TabsTrigger>
          <TabsTrigger value="teams" className="gap-2">
            <Shield className="size-4" />
            <span>Teams</span>
            <span className="rounded bg-surface-raised px-1.5 py-0.5 text-2xs font-mono text-fg-muted">
              {teams.length}
            </span>
          </TabsTrigger>
          <TabsTrigger value="tokens" className="gap-2">
            <Key className="size-4" />
            <span>Service Tokens</span>
            <span className="rounded bg-surface-raised px-1.5 py-0.5 text-2xs font-mono text-fg-muted">
              {serviceTokens.length}
            </span>
          </TabsTrigger>
          {isOwner ? (
            <TabsTrigger value="security" className="gap-2">
              <Lock className="size-4" />
              <span>Security</span>
            </TabsTrigger>
          ) : null}
        </TabsList>

        {/* Members & Invitations Tab */}
        <TabsContent value="members" className="grid gap-8">
          {!hasProjectAdminScope ? (
            <div role="status" className="rounded-md border border-border bg-surface p-6 text-sm">
              <h3 className="font-semibold text-fg">
                Sign in with email or Google to manage organization settings
              </h3>
              <p className="mt-1 text-fg-muted">
                You are currently signed in with a personal access token. Managing organization
                settings requires signing in with email or Google.
              </p>
            </div>
          ) : (
            <>
              {/* Members section */}
              <div className="grid gap-4">
                <div className="flex items-center justify-between">
                  <div>
                    <h2 className="text-sm font-semibold text-fg">Members</h2>
                    <p className="text-xs text-fg-muted">
                      Users who belong to {org?.title ?? 'the organization'}.
                    </p>
                  </div>
                  <Button onClick={() => setInviteOpen(true)} size="sm" className="gap-1.5">
                    <Mail className="size-3.5" />
                    <span>Invite member</span>
                  </Button>
                </div>

                <div className="overflow-x-auto rounded-lg border border-border bg-surface">
                  <table className="w-full border-collapse text-left text-xs">
                    <thead>
                      <tr className="border-b border-border bg-surface-raised font-medium text-fg-muted">
                        <th scope="col" className="px-4 py-2.5 font-semibold">
                          User
                        </th>
                        <th scope="col" className="px-4 py-2.5 font-semibold">
                          Role
                        </th>
                        <th scope="col" className="px-4 py-2.5 font-semibold">
                          Status
                        </th>
                        <th scope="col" className="px-4 py-2.5 font-semibold text-right">
                          Actions
                        </th>
                      </tr>
                    </thead>
                    <tbody>
                      {users.map((user) => (
                        <tr
                          key={user.id}
                          className="border-b border-border/60 last:border-b-0 hover:bg-surface-raised/40 transition-colors"
                        >
                          <td className="px-4 py-3">
                            <div className="flex flex-col">
                              <span className="font-medium text-fg">{user.name}</span>
                              <span className="font-mono text-2xs text-fg-muted">{user.id}</span>
                            </div>
                          </td>
                          <td className="px-4 py-3">
                            <span className="inline-flex rounded border border-border px-2 py-0.5 font-mono text-2xs capitalize text-fg">
                              {user.role}
                            </span>
                          </td>
                          <td className="px-4 py-3">
                            {user.active ? (
                              <span className="inline-flex items-center rounded-full bg-verdict-pass-bg px-2 py-0.5 text-2xs font-medium text-verdict-pass-fg">
                                Active
                              </span>
                            ) : (
                              <span className="inline-flex items-center rounded-full bg-verdict-fail-bg px-2 py-0.5 text-2xs font-medium text-verdict-fail-fg">
                                Deactivated
                              </span>
                            )}
                          </td>
                          <td className="px-4 py-3 text-right">
                            <div className="flex justify-end gap-2">
                              {isOwner && user.id !== me.data?.user?.id ? (
                                <Button
                                  variant="secondary"
                                  size="sm"
                                  disabled={resetMfaMutation.isPending}
                                  aria-label={`Reset MFA for ${user.name}`}
                                  onClick={() => setResetTarget(user)}
                                >
                                  Reset MFA
                                </Button>
                              ) : null}
                              {user.id !== me.data?.user?.id && user.role !== 'owner' ? (
                                <Button
                                  variant="secondary"
                                  size="sm"
                                  disabled={updateUserMutation.isPending}
                                  onClick={() =>
                                    void updateUserMutation.mutateAsync({
                                      userId: user.id,
                                      active: !user.active,
                                    })
                                  }
                                >
                                  {user.active ? 'Deactivate' : 'Reactivate'}
                                </Button>
                              ) : null}
                            </div>
                          </td>
                        </tr>
                      ))}
                      {users.length === 0 ? (
                        <tr>
                          <td colSpan={4} className="px-4 py-6 text-center text-fg-muted">
                            No members found.
                          </td>
                        </tr>
                      ) : null}
                    </tbody>
                  </table>
                </div>
              </div>

              {/* Pending Invitations section */}
              <div className="grid gap-4 border-t border-border pt-6">
                <div>
                  <h2 className="text-sm font-semibold text-fg">Pending invitations</h2>
                  <p className="text-xs text-fg-muted">
                    Invitations sent by email awaiting redemption.
                  </p>
                </div>

                <div className="overflow-x-auto rounded-lg border border-border bg-surface">
                  <table className="w-full border-collapse text-left text-xs">
                    <thead>
                      <tr className="border-b border-border bg-surface-raised font-medium text-fg-muted">
                        <th scope="col" className="px-4 py-2.5 font-semibold">
                          Email
                        </th>
                        <th scope="col" className="px-4 py-2.5 font-semibold">
                          Role
                        </th>
                        <th scope="col" className="px-4 py-2.5 font-semibold">
                          Project grants
                        </th>
                        <th scope="col" className="px-4 py-2.5 font-semibold">
                          Expires
                        </th>
                        <th scope="col" className="px-4 py-2.5 font-semibold text-right">
                          Actions
                        </th>
                      </tr>
                    </thead>
                    <tbody>
                      {invitations.map((inv) => (
                        <tr
                          key={inv.id}
                          className="border-b border-border/60 last:border-b-0 hover:bg-surface-raised/40 transition-colors"
                        >
                          <td className="px-4 py-3 font-medium text-fg">{inv.email}</td>
                          <td className="px-4 py-3">
                            <span className="inline-flex rounded border border-border px-2 py-0.5 font-mono text-2xs capitalize text-fg">
                              {inv.role}
                            </span>
                          </td>
                          <td className="px-4 py-3">
                            {inv.projects.length > 0 ? (
                              <div className="flex flex-wrap gap-1">
                                {inv.projects.map((g) => (
                                  <span
                                    key={g.project}
                                    className="rounded bg-surface-raised px-1.5 py-0.5 text-2xs text-fg-muted font-mono"
                                  >
                                    {g.project} ({g.role})
                                  </span>
                                ))}
                              </div>
                            ) : (
                              <span className="text-fg-muted">—</span>
                            )}
                          </td>
                          <td className="px-4 py-3 text-fg-muted">
                            {formatRelativeTime(inv.expires_at_ms)}
                          </td>
                          <td className="px-4 py-3 text-right">
                            <Button
                              variant="secondary"
                              size="sm"
                              disabled={revokeInvitationMutation.isPending}
                              onClick={() => void revokeInvitationMutation.mutateAsync(inv.id)}
                              aria-label={`Revoke invitation for ${inv.email}`}
                            >
                              Revoke
                            </Button>
                          </td>
                        </tr>
                      ))}
                      {invitations.length === 0 ? (
                        <tr>
                          <td colSpan={5} className="px-4 py-6 text-center text-fg-muted">
                            No pending invitations.
                          </td>
                        </tr>
                      ) : null}
                    </tbody>
                  </table>
                </div>
              </div>
            </>
          )}
        </TabsContent>

        {/* Teams Tab */}
        <TabsContent value="teams" className="grid gap-6">
          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-sm font-semibold text-fg">Teams</h2>
              <p className="text-xs text-fg-muted">
                Group members into operational and review teams.
              </p>
            </div>
            {hasProjectAdminScope ? (
              <Button onClick={() => setCreateTeamOpen(true)} size="sm" className="gap-1.5">
                <Plus className="size-3.5" />
                <span>New team</span>
              </Button>
            ) : null}
          </div>

          {!hasProjectAdminScope ? (
            <div role="status" className="rounded-md border border-border bg-surface p-6 text-sm">
              <h3 className="font-semibold text-fg">
                Sign in with email or Google to manage organization settings
              </h3>
              <p className="mt-1 text-fg-muted">
                You are currently signed in with a personal access token. Managing organization
                settings requires signing in with email or Google.
              </p>
            </div>
          ) : (
            <div className="grid gap-4">
              {teams.map((team) => (
                <TeamCard
                  key={team.id}
                  team={team}
                  orgId={orgId}
                  allUsers={users}
                  nameOf={nameOf}
                />
              ))}
              {teams.length === 0 ? (
                <div className="rounded-lg border border-border bg-surface p-8 text-center text-fg-muted text-xs">
                  No teams created yet. Click &ldquo;New team&rdquo; to create your first team.
                </div>
              ) : null}
            </div>
          )}
        </TabsContent>

        <TabsContent value="security">
          <OrgSecurityTab orgId={orgId} isOwner={isOwner} />
        </TabsContent>

        <TabsContent value="projects">
          <OrgProjectsTab canEdit={isOwnerOrAdmin && hasProjectAdminScope} />
        </TabsContent>

        {/* Service Tokens Tab */}
        <TabsContent value="tokens" className="grid gap-4">
          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-sm font-semibold text-fg">Service tokens</h2>
              <p className="text-xs text-fg-muted">
                Project-scoped credentials for automation, CI/CD, and scripts.
              </p>
            </div>
            {hasTokensManageScope ? (
              <Button onClick={() => setCreateTokenOpen(true)} size="sm" className="gap-1.5">
                <Plus className="size-3.5" />
                <span>New service token</span>
              </Button>
            ) : null}
          </div>

          {!hasTokensManageScope ? (
            <div role="status" className="rounded-md border border-border bg-surface p-6 text-sm">
              <h3 className="font-semibold text-fg">
                Sign in with email or Google to manage service tokens
              </h3>
              <p className="mt-1 text-fg-muted">
                You are currently signed in with a personal access token. Managing service tokens
                requires signing in with email or Google.
              </p>
            </div>
          ) : (
            <div className="overflow-x-auto rounded-lg border border-border bg-surface">
              <table className="w-full border-collapse text-left text-xs">
                <thead>
                  <tr className="border-b border-border bg-surface-raised font-medium text-fg-muted">
                    <th scope="col" className="px-4 py-2.5 font-semibold">
                      Name
                    </th>
                    <th scope="col" className="px-4 py-2.5 font-semibold">
                      Project
                    </th>
                    <th scope="col" className="px-4 py-2.5 font-semibold">
                      Role
                    </th>
                    <th scope="col" className="px-4 py-2.5 font-semibold">
                      Scopes
                    </th>
                    <th scope="col" className="px-4 py-2.5 font-semibold">
                      Last used
                    </th>
                    <th scope="col" className="px-4 py-2.5 font-semibold">
                      Expires
                    </th>
                    <th scope="col" className="px-4 py-2.5 font-semibold text-right">
                      Actions
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {serviceTokens.map((token) => (
                    <tr
                      key={token.id}
                      className="border-b border-border/60 last:border-b-0 hover:bg-surface-raised/40 transition-colors"
                    >
                      <td className="px-4 py-3 font-medium text-fg">
                        <div className="flex flex-col">
                          <span>{token.name}</span>
                          <span className="font-mono text-2xs text-fg-muted">{token.id}</span>
                          {serviceTokensNeedProof && token.fresh_mfa_at_issue !== true ? (
                            <TokenReplacementBadge guidance="Create a new service token here (you will confirm it is you), move the pipeline to it, then revoke this one." />
                          ) : null}
                        </div>
                      </td>
                      <td className="px-4 py-3 text-fg">
                        {token.projects?.join(', ') ?? 'All projects'}
                      </td>
                      <td className="px-4 py-3">
                        <span className="inline-flex rounded border border-border px-2 py-0.5 font-mono text-2xs capitalize text-fg">
                          {token.role ?? 'default'}
                        </span>
                      </td>
                      <td className="px-4 py-3">
                        <div className="flex flex-wrap gap-1 max-w-xs">
                          {token.scopes.map((s) => (
                            <span
                              key={s}
                              className="rounded bg-surface-raised px-1 py-0.5 font-mono text-2xs text-fg-muted"
                            >
                              {s}
                            </span>
                          ))}
                        </div>
                      </td>
                      <td className="px-4 py-3 text-fg-muted">
                        {token.last_used_at_ms
                          ? formatRelativeTime(token.last_used_at_ms)
                          : 'Never'}
                      </td>
                      <td className="px-4 py-3 text-fg-muted">
                        {token.expires_at_ms ? formatRelativeTime(token.expires_at_ms) : 'Never'}
                      </td>
                      <td className="px-4 py-3 text-right">
                        <Button
                          variant="secondary"
                          size="sm"
                          disabled={revokeTokenMutation.isPending}
                          onClick={() => void revokeTokenMutation.mutateAsync(token.id)}
                          aria-label={`Revoke ${token.name}`}
                        >
                          Revoke
                        </Button>
                      </td>
                    </tr>
                  ))}
                  {serviceTokens.length === 0 ? (
                    <tr>
                      <td colSpan={7} className="px-4 py-6 text-center text-fg-muted">
                        No service tokens configured.
                      </td>
                    </tr>
                  ) : null}
                </tbody>
              </table>
            </div>
          )}
        </TabsContent>
      </Tabs>

      <ConfirmDialog
        open={resetTarget !== null}
        onOpenChange={(open) => (open ? undefined : setResetTarget(null))}
        title={`Reset ${resetTarget?.name ?? 'this member'}'s second factors?`}
        description="This removes their passkeys, authenticator app and backup codes, signs them out everywhere and revokes their app authorizations. They get an email and set up a new factor when they next sign in."
        confirmLabel="Reset MFA"
        destructive
        onConfirm={() => {
          const target = resetTarget;
          setResetTarget(null);
          if (target) void resetMfa(target);
        }}
      />

      <InviteMemberDialog open={inviteOpen} onOpenChange={setInviteOpen} />
      <CreateTeamDialog open={createTeamOpen} onOpenChange={setCreateTeamOpen} />
      <CreateServiceTokenDialog open={createTokenOpen} onOpenChange={setCreateTokenOpen} />
    </div>
  );
}

function TeamCard({
  team,
  orgId,
  allUsers,
  nameOf,
}: {
  team: TeamView;
  orgId: string | undefined;
  allUsers: DirectoryUserView[];
  nameOf: (id: string) => string;
}) {
  const membersQuery = useTeamMembers(orgId, team.id);
  const addMemberMutation = useAddTeamMember(orgId, team.id);
  const removeMemberMutation = useRemoveTeamMember(orgId, team.id);

  const [selectedUser, setSelectedUser] = useState<string>('');

  const members = useMemo(() => membersQuery.data ?? [], [membersQuery.data]);

  const availableUsers = useMemo(
    () => allUsers.filter((u) => !members.some((m) => m.user_id === u.id)),
    [allUsers, members],
  );

  async function handleAdd() {
    if (!selectedUser) return;
    await addMemberMutation.mutateAsync(selectedUser);
    setSelectedUser('');
  }

  return (
    <div className="rounded-lg border border-border bg-surface p-4">
      <div className="flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between border-b border-border pb-3">
        <div>
          <h3 className="text-sm font-semibold text-fg">{team.title}</h3>
          <p className="font-mono text-2xs text-fg-muted">{team.slug}</p>
        </div>
        <div className="flex items-center gap-2 pt-2 sm:pt-0">
          <Select value={selectedUser} onValueChange={setSelectedUser}>
            <SelectTrigger className="w-48 text-xs">
              <SelectValue placeholder="Add member..." />
            </SelectTrigger>
            <SelectContent>
              {availableUsers.map((u) => (
                <SelectItem key={u.id} value={u.id}>
                  {u.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button
            size="sm"
            variant="secondary"
            onClick={() => void handleAdd()}
            disabled={!selectedUser || addMemberMutation.isPending}
            aria-label={`Add user to ${team.title}`}
          >
            <UserPlus className="size-3.5" />
          </Button>
        </div>
      </div>

      <div className="pt-3">
        <span className="text-2xs font-medium text-fg-muted uppercase tracking-wider">
          Members ({members.length})
        </span>
        <div className="mt-2 flex flex-wrap gap-2">
          {members.map((m) => (
            <span
              key={m.user_id}
              className="inline-flex items-center gap-1.5 rounded-md border border-border bg-surface-raised px-2.5 py-1 text-xs text-fg"
            >
              <span>{nameOf(m.user_id)}</span>
              <button
                type="button"
                onClick={() => void removeMemberMutation.mutateAsync(m.user_id)}
                disabled={removeMemberMutation.isPending}
                className="text-fg-muted hover:text-fg"
                aria-label={`Remove ${nameOf(m.user_id)} from ${team.title}`}
              >
                <X className="size-3" />
              </button>
            </span>
          ))}
          {members.length === 0 ? (
            <span className="text-xs text-fg-muted italic">No team members yet.</span>
          ) : null}
        </div>
      </div>
    </div>
  );
}
