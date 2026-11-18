import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, type Schemas } from '../../api/client';

export type SkillVersionView = Schemas['SkillVersionView'];
export type DirectoryPage_SkillVersionView = Schemas['DirectoryPage_SkillVersionView'];

export const projectSkillsKey = (projectId: string | undefined) =>
  ['projects', projectId ?? '', 'skills'] as const;

export const skillDetailKey = (projectId: string | undefined, hash: string | undefined) =>
  ['projects', projectId ?? '', 'skills', hash ?? ''] as const;

export function useProjectSkills(projectId: string | undefined) {
  return useQuery({
    queryKey: projectSkillsKey(projectId),
    enabled: Boolean(projectId),
    queryFn: async () => {
      const { data } = await api.GET('/v1/projects/{p}/skills', {
        params: { path: { p: projectId ?? '' } },
      });
      return (data as DirectoryPage_SkillVersionView) ?? { items: [] };
    },
  });
}

export function useSkillDetail(projectId: string | undefined, hash: string | undefined) {
  return useQuery({
    queryKey: skillDetailKey(projectId, hash),
    enabled: Boolean(projectId && hash),
    queryFn: async () => {
      const { data } = await api.GET('/v1/projects/{p}/skills/{hash}', {
        params: {
          path: { p: projectId ?? '', hash: hash ?? '' },
          query: { files: true },
        },
      });
      return data as SkillVersionView;
    },
  });
}

export function decodeBase64Utf8(b64: string): string {
  try {
    const bin = atob(b64);
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    return new TextDecoder().decode(bytes);
  } catch {
    return atob(b64);
  }
}

export function useUploadSkill(projectId: string | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (request: Schemas['SkillUploadRequest']) => {
      const { data, error } = await api.POST('/v1/projects/{p}/skills', {
        params: {
          path: { p: projectId as string },
          header: { 'Idempotency-Key': globalThis.crypto.randomUUID() },
        },
        body: request,
      });
      if (error) throw error;
      return data as SkillVersionView;
    },
    onSuccess: (uploaded) => {
      void queryClient.invalidateQueries({
        queryKey: projectSkillsKey(projectId),
      });
      return uploaded;
    },
  });
}
