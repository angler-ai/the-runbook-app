// Profile and project settings (2026-11-17): a person's name and photo, a
// project's name and image.
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { api } from '../../api/client';
import { queryKeys } from '../../api/query-keys';
import { bumpPictures } from '../../ui/status/PictureAvatar';

export function useUpdateMyName() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: async (name: string) => {
      const { data } = await api.PATCH('/v1/me', { body: { name } });
      return data;
    },
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: queryKeys.me });
      // Directory reads (names of people) pick the new name up too.
      void client.invalidateQueries({ queryKey: ['orgs'] });
    },
  });
}

export function useMyPicture() {
  return useMutation({
    mutationFn: async (data: string | null) => {
      if (data === null) await api.DELETE('/v1/me/picture');
      else await api.PUT('/v1/me/picture', { body: { data } });
    },
    onSuccess: () => bumpPictures(),
  });
}

export function useUpdateProject(project: string) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: async (title: string) => {
      const { data } = await api.PATCH('/v1/projects/{p}', {
        params: { path: { p: project } },
        body: { title },
      });
      return data;
    },
    onSuccess: () => void client.invalidateQueries({ queryKey: ['orgs'] }),
  });
}

export function useProjectPicture(project: string) {
  return useMutation({
    mutationFn: async (data: string | null) => {
      const params = { path: { p: project } };
      if (data === null) await api.DELETE('/v1/projects/{p}/picture', { params });
      else await api.PUT('/v1/projects/{p}/picture', { params, body: { data } });
    },
    onSuccess: () => bumpPictures(),
  });
}
