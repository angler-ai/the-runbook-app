import { useMemo, useState } from 'react';
import { useParams } from '@tanstack/react-router';
import { PageHeader } from '../../ui/layout/PageHeader';
import { ProjectProfileForm } from '../profile/ProjectProfileForm';
import { Button } from '../../ui/primitives/button';
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
import {
  friendlyAdminError,
  useGrantProjectMember,
  useProjectMembers,
  useRevokeProjectMember,
  type ProjectRole,
} from './hooks';
import { Shield, UserPlus, X } from 'lucide-react';
import { SlackSection } from '../settings/SlackSection';

export interface ProjectSettingsPageProps {
  project?: string;
}

export function ProjectSettingsPage({ project: propProject }: ProjectSettingsPageProps = {}) {
  let paramProject: string | undefined;
  try {
    const params = useParams({ strict: false }) as { project?: string };
    paramProject = params?.project;
  } catch {
    // Outside RouterProvider
  }
  const me = useMe();
  const { projects, directory } = useOrgContext();
  const { nameOf } = useDirectoryNames();

  const projectParam = propProject ?? paramProject ?? projects[0]?.slug;
  const currentProject = useMemo(
    () => projects.find((p) => p.id === projectParam || p.slug === projectParam),
    [projects, projectParam],
  );

  const projectId = currentProject?.id;
  const hasProjectAdminScope = Boolean(me.data?.credential.scopes?.includes('project:admin'));
  const membersQuery = useProjectMembers(projectId, { enabled: hasProjectAdminScope });
  const grantMutation = useGrantProjectMember(projectId);
  const revokeMutation = useRevokeProjectMember(projectId);

  const [selectedUser, setSelectedUser] = useState<string>('');
  const [selectedRole, setSelectedRole] = useState<ProjectRole>('editor');
  const [error, setError] = useState<string | null>(null);

  const members = useMemo(() => membersQuery.data ?? [], [membersQuery.data]);
  const currentUserId = me.data?.user?.id;
  const isOrgAdmin = me.data?.org.role === 'owner' || me.data?.org.role === 'admin';
  const myProjectRole = members.find((m) => m.user_id === currentUserId)?.role;
  const canManage = isOrgAdmin || myProjectRole === 'admin';

  const availableUsers = useMemo(
    () => directory.users.filter((u) => !members.some((m) => m.user_id === u.id)),
    [directory.users, members],
  );

  async function handleAddMember() {
    if (!selectedUser) return;
    setError(null);
    try {
      await grantMutation.mutateAsync({
        userId: selectedUser,
        role: selectedRole,
      });
      setSelectedUser('');
    } catch (err) {
      setError(friendlyAdminError(err));
    }
  }

  async function handleRoleChange(userId: string, newRole: ProjectRole) {
    setError(null);
    try {
      await grantMutation.mutateAsync({
        userId,
        role: newRole,
      });
    } catch (err) {
      setError(friendlyAdminError(err));
    }
  }

  async function handleRemoveMember(userId: string) {
    setError(null);
    try {
      await revokeMutation.mutateAsync(userId);
    } catch (err) {
      setError(friendlyAdminError(err));
    }
  }

  if (!currentProject) {
    return (
      <div className="w-full">
        <PageHeader title="Project settings" description="Project not found." />
      </div>
    );
  }

  if (me.data && !hasProjectAdminScope) {
    return (
      <div className="w-full">
        <PageHeader
          title={`${currentProject.title} settings`}
          description={`Manage project membership and access controls for ${currentProject.title} (${currentProject.slug}).`}
        />
        <div role="status" className="mt-6 rounded-md border border-border bg-surface p-6 text-sm">
          <h3 className="font-semibold text-fg">
            Sign in with email or Google to manage project settings
          </h3>
          <p className="mt-1 text-fg-muted">
            You are currently signed in with a personal access token. Managing project settings
            requires signing in with email or Google.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="w-full">
      <PageHeader
        title={`${currentProject.title} settings`}
        description={`Manage project membership and access controls for ${currentProject.title} (${currentProject.slug}).`}
      />

      <div className="grid gap-6">
        <section
          aria-label="Project name and image"
          className="rounded-lg border border-border bg-surface p-6"
        >
          <h2 className="mb-4 text-sm font-semibold text-fg">Name and image</h2>
          <ProjectProfileForm
            key={currentProject.id + currentProject.title}
            projectId={currentProject.id}
            slug={currentProject.slug}
            title={currentProject.title}
            canEdit={canManage && hasProjectAdminScope}
          />
        </section>
        <div className="rounded-lg border border-border bg-surface p-6">
          <SlackSection projectRef={currentProject.slug} />
        </div>
        <div className="rounded-lg border border-border bg-surface p-6">
          <div className="flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between border-b border-border pb-4">
            <div>
              <h2 className="text-sm font-semibold text-fg">Project members</h2>
              <p className="text-xs text-fg-muted">
                Users with direct role assignments in this project.
              </p>
            </div>

            {canManage ? (
              <div className="flex items-center gap-2 pt-2 sm:pt-0">
                <Select value={selectedUser} onValueChange={setSelectedUser}>
                  <SelectTrigger className="w-48 text-xs" aria-label="Select user to add">
                    <SelectValue placeholder="Select user..." />
                  </SelectTrigger>
                  <SelectContent>
                    {availableUsers.map((u) => (
                      <SelectItem key={u.id} value={u.id}>
                        {u.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <Select
                  value={selectedRole}
                  onValueChange={(val) => setSelectedRole(val as ProjectRole)}
                >
                  <SelectTrigger className="w-28 text-xs" aria-label="Select project role">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="viewer">Viewer</SelectItem>
                    <SelectItem value="operator">Operator</SelectItem>
                    <SelectItem value="editor">Editor</SelectItem>
                    <SelectItem value="admin">Admin</SelectItem>
                  </SelectContent>
                </Select>
                <Button
                  size="sm"
                  onClick={() => void handleAddMember()}
                  disabled={!selectedUser || grantMutation.isPending}
                  aria-label="Add project member"
                >
                  <UserPlus className="size-3.5 mr-1" />
                  <span>Add</span>
                </Button>
              </div>
            ) : null}
          </div>

          {error ? (
            <p role="alert" className="mt-3 text-xs text-verdict-fail-fg font-medium">
              {error}
            </p>
          ) : null}

          {!canManage ? (
            <div className="mt-4 rounded border border-border bg-surface-raised p-3 text-xs text-fg-muted flex items-center gap-2">
              <Shield className="size-4 shrink-0 text-fg-muted" />
              <span>
                You have read-only access to project membership. Ask a project administrator to
                change members.
              </span>
            </div>
          ) : null}

          <div className="mt-4 overflow-x-auto rounded-lg border border-border bg-surface">
            <table className="w-full border-collapse text-left text-xs">
              <thead>
                <tr className="border-b border-border bg-surface-raised font-medium text-fg-muted">
                  <th scope="col" className="px-4 py-2.5 font-semibold">
                    User
                  </th>
                  <th scope="col" className="px-4 py-2.5 font-semibold">
                    Role
                  </th>
                  {canManage ? (
                    <th scope="col" className="px-4 py-2.5 font-semibold text-right">
                      Actions
                    </th>
                  ) : null}
                </tr>
              </thead>
              <tbody>
                {members.map((member) => (
                  <tr
                    key={member.user_id}
                    className="border-b border-border/60 last:border-b-0 hover:bg-surface-raised/40 transition-colors"
                  >
                    <td className="px-4 py-3">
                      <div className="flex flex-col">
                        <span className="font-medium text-fg">{nameOf(member.user_id)}</span>
                        <span className="font-mono text-2xs text-fg-muted">{member.user_id}</span>
                      </div>
                    </td>
                    <td className="px-4 py-3">
                      {canManage ? (
                        <Select
                          value={member.role}
                          onValueChange={(val) =>
                            void handleRoleChange(member.user_id, val as ProjectRole)
                          }
                          disabled={grantMutation.isPending}
                        >
                          <SelectTrigger
                            className="w-28 text-xs"
                            aria-label={`Change role for ${nameOf(member.user_id)}`}
                          >
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            <SelectItem value="viewer">Viewer</SelectItem>
                            <SelectItem value="operator">Operator</SelectItem>
                            <SelectItem value="editor">Editor</SelectItem>
                            <SelectItem value="admin">Admin</SelectItem>
                          </SelectContent>
                        </Select>
                      ) : (
                        <span className="inline-flex rounded border border-border px-2 py-0.5 font-mono text-2xs capitalize text-fg">
                          {member.role}
                        </span>
                      )}
                    </td>
                    {canManage ? (
                      <td className="px-4 py-3 text-right">
                        <Button
                          variant="secondary"
                          size="sm"
                          disabled={revokeMutation.isPending}
                          onClick={() => void handleRemoveMember(member.user_id)}
                          aria-label={`Remove ${nameOf(member.user_id)} from project`}
                        >
                          <X className="size-3.5 mr-1" />
                          <span>Remove</span>
                        </Button>
                      </td>
                    ) : null}
                  </tr>
                ))}
                {members.length === 0 ? (
                  <tr>
                    <td colSpan={canManage ? 3 : 2} className="px-4 py-6 text-center text-fg-muted">
                      No members assigned to this project yet.
                    </td>
                  </tr>
                ) : null}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </div>
  );
}
