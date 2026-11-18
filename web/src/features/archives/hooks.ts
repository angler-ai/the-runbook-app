import { useQuery } from '@tanstack/react-query';
import { api, type Schemas } from '../../api/client';

export type ArchiveView = Schemas['ArchiveView'];
export type ArchivePage = Schemas['ArchivePage'];

export const runbookArchivesKey = (runbookId: string) =>
  ['runbooks', runbookId, 'archives'] as const;

export function useRunbookArchives(runbookId: string) {
  return useQuery({
    queryKey: runbookArchivesKey(runbookId),
    enabled: Boolean(runbookId),
    queryFn: async () => {
      const { data } = await api.GET('/v1/runbooks/{rb}/archives', {
        params: { path: { rb: runbookId } },
      });
      return (data as ArchivePage) ?? { items: [] };
    },
  });
}

const REVOKE_AFTER_MS = 1_000;

export async function downloadArchive(runbookId: string, date: string): Promise<void> {
  const { data } = await api.GET('/v1/runbooks/{rb}/archives/{date}', {
    params: { path: { rb: runbookId, date } },
    parseAs: 'blob',
  });
  const blob = (data as unknown as Blob) ?? new Blob([]);
  const url = URL.createObjectURL(blob);
  try {
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = `${runbookId}-${date}.zip`;
    anchor.click();
  } finally {
    setTimeout(() => URL.revokeObjectURL(url), REVOKE_AFTER_MS);
  }
}
