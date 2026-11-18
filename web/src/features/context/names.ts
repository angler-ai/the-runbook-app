// Directory names for user and team ids (walkthrough #1), from the M1
// directory cache: the org's users and teams, read once and shared through
// the React Query cache with OrgContext. Works without an OrgProvider, so
// any page (or test) can use it. An id the directory does not know shows
// as a short id, never a crash and never the raw 30 characters.
import { useMemo } from 'react';
import { useDirectoryTeams, useDirectoryUsers, useMe } from '../../api/hooks';
import { shortId, type NameOf } from '../../ui/execution/names';

export interface DirectoryNames {
  nameOf: NameOf;
  /** A team reference (its id or slug) as its slug. */
  teamName(reference: string): string;
}

export function useDirectoryNames(): DirectoryNames {
  const me = useMe();
  const orgId = me.data?.org.id;
  const users = useDirectoryUsers(orgId);
  const teams = useDirectoryTeams(orgId);
  const self = me.data?.user;
  return useMemo(() => {
    const byId = new Map((users.data ?? []).map((user) => [user.id, user.name]));
    // The signed-in person is always known, even before the directory loads.
    if (self && !byId.has(self.id)) byId.set(self.id, self.name);
    const teamsById = new Map((teams.data ?? []).map((team) => [team.id, team.slug]));
    return {
      nameOf: (userId) => byId.get(userId) ?? shortId(userId),
      teamName: (reference) => teamsById.get(reference) ?? reference,
    };
  }, [users.data, teams.data, self]);
}
