import { useState } from 'react';
import { Archive, Download, Loader2, AlertCircle } from 'lucide-react';
import { ApiError } from '../../api/client';
import { EmptyState, ErrorState, Skeleton } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import { formatRelativeTime } from '../../lib/format';
import { useRunbookArchives, downloadArchive, type ArchiveView } from './hooks';

export interface RunbookArchivesTabProps {
  runbookId: string;
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function RunbookArchivesTab({ runbookId }: RunbookArchivesTabProps) {
  const archives = useRunbookArchives(runbookId);
  const [downloadingDate, setDownloadingDate] = useState<string | null>(null);
  const [downloadError, setDownloadError] = useState<string | null>(null);

  if (archives.isLoading) {
    return (
      <div className="grid gap-3" aria-busy="true" aria-label="Loading archives">
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-32 w-full" />
      </div>
    );
  }

  if (archives.isError) {
    const error = archives.error;
    return (
      <ErrorState
        title="Could not load daily archives"
        message={
          error instanceof Error ? error.message : 'An error occurred while fetching archives.'
        }
        code={error instanceof ApiError ? error.code : undefined}
        requestId={error instanceof ApiError ? (error.requestId ?? undefined) : undefined}
        onRetry={() => void archives.refetch()}
      />
    );
  }

  const items = archives.data?.items ?? [];

  if (items.length === 0) {
    return (
      <EmptyState
        icon={Archive}
        title="No daily archives"
        description="Daily archives are generated automatically by the retention and archive job once runs complete."
      />
    );
  }

  async function handleDownload(archive: ArchiveView) {
    setDownloadError(null);
    setDownloadingDate(archive.date);
    try {
      await downloadArchive(runbookId, archive.date);
    } catch (err) {
      setDownloadError(err instanceof Error ? err.message : 'Download failed.');
    } finally {
      setDownloadingDate(null);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h2 className="text-sm font-semibold text-fg">Daily archives</h2>
          <p className="text-xs text-fg-muted">
            Deterministic daily ZIP archives containing execution manifests, reports, and portable
            contexts.
          </p>
        </div>
      </div>

      {downloadError ? (
        <div
          role="alert"
          className="rounded-lg border border-verdict-fail-border bg-verdict-fail-bg p-3 text-xs text-verdict-fail-fg flex items-center gap-2"
        >
          <AlertCircle className="size-4 shrink-0" aria-hidden="true" />
          <span>{downloadError}</span>
        </div>
      ) : null}

      <div className="overflow-x-auto rounded-lg border border-border bg-surface">
        <table
          className="w-full border-collapse text-left text-xs"
          aria-label="Daily archives table"
        >
          <thead>
            <tr className="border-b border-border bg-surface-raised font-medium text-fg-muted">
              <th scope="col" className="px-4 py-2.5 font-semibold">
                Date
              </th>
              <th scope="col" className="px-4 py-2.5 font-semibold">
                Runs
              </th>
              <th scope="col" className="px-4 py-2.5 font-semibold">
                Size
              </th>
              <th scope="col" className="px-4 py-2.5 font-semibold">
                SHA-256
              </th>
              <th scope="col" className="px-4 py-2.5 font-semibold">
                Written
              </th>
              <th scope="col" className="px-4 py-2.5 font-semibold text-right">
                Actions
              </th>
            </tr>
          </thead>
          <tbody>
            {items.map((archive) => {
              const isDownloading = downloadingDate === archive.date;
              return (
                <tr
                  key={archive.date}
                  className="border-b border-border/60 last:border-b-0 hover:bg-surface-raised/40 transition-colors"
                >
                  <td className="px-4 py-3">
                    <div className="flex items-center gap-2">
                      <span className="font-mono font-medium text-fg">{archive.date}</span>
                      {archive.stale ? (
                        <span
                          className="inline-flex items-center rounded bg-verdict-warn-bg px-1.5 py-0.5 text-2xs font-medium text-verdict-warn-fg border border-verdict-warn-border"
                          title="An amendment or deletion is waiting for the next daily rewrite"
                        >
                          Rewrite pending
                        </span>
                      ) : null}
                    </div>
                  </td>
                  <td className="px-4 py-3 text-fg">
                    {archive.run_count} {archive.run_count === 1 ? 'run' : 'runs'}
                  </td>
                  <td className="px-4 py-3 font-mono text-2xs text-fg-muted">
                    {formatBytes(archive.size_bytes)}
                  </td>
                  <td className="px-4 py-3 font-mono text-2xs text-fg-muted">
                    <span title={archive.sha256}>{archive.sha256.slice(0, 12)}…</span>
                  </td>
                  <td className="px-4 py-3 text-fg-muted">
                    {formatRelativeTime(archive.written_at_ms)}
                  </td>
                  <td className="px-4 py-3 text-right">
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => void handleDownload(archive)}
                      disabled={isDownloading}
                      aria-label={`Download archive for ${archive.date}`}
                    >
                      {isDownloading ? (
                        <Loader2 className="size-3.5 animate-spin mr-1.5" aria-hidden="true" />
                      ) : (
                        <Download className="size-3.5 mr-1.5" aria-hidden="true" />
                      )}
                      <span>Download</span>
                    </Button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
