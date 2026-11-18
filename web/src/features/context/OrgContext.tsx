// Org/project context and the directory cache (M1). The org comes from
// /v1/me and is confirmed against /v1/orgs; projects come from the org's
// project list. The directory cache resolves user IDs and team IDs to
// display names for later phases, and lives in the React Query cache.
import { createContext, useContext, useMemo, type ReactNode } from 'react';
import {
  useDirectoryTeams,
  useDirectoryUsers,
  useMe,
  useOrganizations,
  useProjects,
  type DirectoryUserView,
  type OrganizationView,
  type ProjectView,
  type TeamView,
} from '../../api/hooks';
import { shortId } from '../../ui/execution/names';

export interface DirectoryCache {
  users: DirectoryUserView[];
  teams: TeamView[];
  userName(userId: string): string;
  teamSlug(teamId: string): string;
}

export interface OrgContextValue {
  /** The signed-in organization, once /v1/orgs confirms it. */
  org: OrganizationView | undefined;
  projects: ProjectView[];
  directory: DirectoryCache;
  isLoading: boolean;
  /** Route params may carry a project slug or ID; resolve either. */
  resolveProject(ref: string | undefined): ProjectView | undefined;
}

const OrgContext = createContext<OrgContextValue | null>(null);

export function OrgProvider({ children }: { children: ReactNode }) {
  const me = useMe();
  const orgId = me.data?.org.id;
  const orgs = useOrganizations();
  const projects = useProjects(orgId);
  const users = useDirectoryUsers(orgId);
  const teams = useDirectoryTeams(orgId);

  const value = useMemo<OrgContextValue>(() => {
    const userList = users.data ?? [];
    const teamList = teams.data ?? [];
    const usersById = new Map(userList.map((u) => [u.id, u]));
    const teamsById = new Map(teamList.map((t) => [t.id, t]));
    const projectList = projects.data ?? [];
    return {
      org: orgs.data?.find((o) => o.id === orgId),
      projects: projectList,
      directory: {
        users: userList,
        teams: teamList,
        userName: (userId) => usersById.get(userId)?.name ?? shortId(userId),
        teamSlug: (teamId) => teamsById.get(teamId)?.slug ?? teamId,
      },
      isLoading: me.isLoading || orgs.isLoading || projects.isLoading,
      resolveProject: (ref) =>
        ref ? projectList.find((p) => p.id === ref || p.slug === ref) : undefined,
    };
  }, [
    me.isLoading,
    orgs.data,
    orgs.isLoading,
    orgId,
    projects.data,
    projects.isLoading,
    teams.data,
    users.data,
  ]);

  return <OrgContext.Provider value={value}>{children}</OrgContext.Provider>;
}

export function useOrgContext(): OrgContextValue {
  const value = useContext(OrgContext);
  if (!value) throw new Error('useOrgContext requires an OrgProvider ancestor');
  return value;
}
