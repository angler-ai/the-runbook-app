import { useQuery } from '@tanstack/react-query';
import { api, type Schemas } from '../../api/client';
import { useMe } from '../../api/hooks';

export type UniversalSearchView = Schemas['UniversalSearchView'];

export const universalSearchKey = (orgId: string, q: string) =>
  ['orgs', orgId, 'search', q] as const;

/** One access-filtered search across the caller's projects (SEARCH2). */
export function useUniversalSearch(q: string) {
  const me = useMe();
  const orgId = me.data?.org.id;
  return useQuery({
    queryKey: universalSearchKey(orgId ?? '', q),
    enabled: Boolean(orgId) && q.trim().length > 0,
    queryFn: async () => {
      const { data } = await api.GET('/v1/orgs/{org}/search', {
        params: { path: { org: orgId as string }, query: { q } },
      });
      return data as UniversalSearchView;
    },
  });
}
