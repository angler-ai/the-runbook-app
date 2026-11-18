// buildCompileContext (M4): the CompileDirectory the browser compiler
// validates against (runbook_spec::authoring::CompileDirectory, which denies
// unknown fields), read through the same /v1 routes the server's facts come
// from: the org's directory users and teams, the project's skill library and
// the project's variable declarations.
//
// The browser's result is provisional (F99): the directory can be stale, and
// a part the token may not read (skills need `skills:read`) is left empty and
// reported as a gap rather than failing the whole editor.
import { useQuery } from '@tanstack/react-query';
import { api, ApiError, type Schemas } from '../../api/client';
import { fetchAllPages } from '../../api/hooks';
import type { CompileDirectory } from '../../wasm/runbook';

interface Page<Item> {
  items: Item[];
  next_cursor?: string | null;
}

/** A directory part the browser could not read; its references go unchecked. */
export type DirectoryGap = 'skills' | 'variables';

export interface CompileContext {
  directory: CompileDirectory;
  gaps: DirectoryGap[];
}

/**
 * One version per skill name, sorted by name: the one the server marks
 * `latest`, which is the version validation and publishing pin when a
 * runbook names the skill (the server's compile context). `GET …/skills`
 * lists versions by `name:hash`, not by recency, so list position says
 * nothing. A name with no version marked latest (not expected from the
 * server) keeps its first listed version, so its references still resolve.
 */
export function latestSkills(
  versions: Schemas['SkillVersionView'][],
): NonNullable<CompileDirectory['skills']> {
  const byName = new Map<string, Schemas['SkillVersionView']>();
  for (const version of versions) {
    const kept = byName.get(version.name);
    if (!kept || (version.latest && !kept.latest)) byName.set(version.name, version);
  }
  return [...byName.values()]
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
    .map((version) => ({
      name: version.name,
      hash: version.hash,
      reference: version.reference ?? null,
    }));
}

/** A read the credential may not make (403) becomes a gap; anything else fails. */
async function optional<T>(read: () => Promise<T>, empty: T): Promise<{ value: T; ok: boolean }> {
  try {
    return { value: await read(), ok: true };
  } catch (error) {
    if (error instanceof ApiError && error.status === 403) return { value: empty, ok: false };
    throw error;
  }
}

export async function buildCompileContext(
  orgId: string,
  projectId: string,
): Promise<CompileContext> {
  const [users, teams, skills, vars] = await Promise.all([
    fetchAllPages(async (cursor) => {
      const { data } = await api.GET('/v1/orgs/{org}/users', {
        params: { path: { org: orgId }, query: { limit: 100, ...(cursor ? { cursor } : {}) } },
      });
      return data as Page<Schemas['DirectoryUserView']>;
    }),
    fetchAllPages(async (cursor) => {
      const { data } = await api.GET('/v1/orgs/{org}/teams', {
        params: { path: { org: orgId }, query: { limit: 100, ...(cursor ? { cursor } : {}) } },
      });
      return data as Page<Schemas['TeamView']>;
    }),
    optional(
      () =>
        fetchAllPages(async (cursor) => {
          const { data } = await api.GET('/v1/projects/{p}/skills', {
            params: {
              path: { p: projectId },
              query: { limit: 100, ...(cursor ? { cursor } : {}) },
            },
          });
          return data as Page<Schemas['SkillVersionView']>;
        }),
      [] as Schemas['SkillVersionView'][],
    ),
    optional(
      async () => {
        const { data } = await api.GET('/v1/projects/{p}/vars', {
          params: { path: { p: projectId } },
        });
        return (data as Schemas['ProjectVariables']).declarations;
      },
      {} as Record<string, unknown>,
    ),
  ]);
  const gaps: DirectoryGap[] = [];
  if (!skills.ok) gaps.push('skills');
  if (!vars.ok) gaps.push('variables');
  return {
    directory: {
      // Exactly the wire shape: no role or other view fields (deny_unknown_fields).
      users: users.map((user) => ({ id: user.id, name: user.name, active: user.active })),
      teams: teams.map((team) => ({ id: team.id, slug: team.slug })),
      skills: latestSkills(skills.value),
      variables: vars.value,
    },
    gaps,
  };
}

export const compileContextKey = (orgId: string, projectId: string) =>
  ['projects', projectId, 'compile-context', orgId] as const;

export function useCompileContext(orgId: string | undefined, projectId: string | undefined) {
  return useQuery({
    queryKey: compileContextKey(orgId ?? '', projectId ?? ''),
    enabled: Boolean(orgId && projectId),
    queryFn: () => buildCompileContext(orgId as string, projectId as string),
    // Directory edits are rare; refetch on focus keeps the browser close to the server.
    staleTime: 30_000,
  });
}
