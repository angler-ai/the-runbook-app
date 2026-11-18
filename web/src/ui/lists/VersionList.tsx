import type { ReactNode } from 'react';
import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { EmptyState } from '../layout/EmptyState';
import { Button } from '../primitives/button';
import { MonoRef } from '../status/MonoRef';
import { formatRelativeTime } from '../../lib/format';
import { GitBranch, GitCommit, History, Loader2, ArrowLeftRight } from 'lucide-react';

export type VersionView = components['schemas']['VersionView'] & {
  created_at_ms?: number | null;
  author?: string | null;
  message?: string | null;
};

export interface VersionListProps {
  versions: VersionView[];
  selectedVersionId?: string;
  onSelectVersion?: (version: VersionView) => void;
  onCompare?: (v1: VersionView, v2: VersionView) => void;
  onLoadMore?: () => void;
  hasMore?: boolean;
  isLoadingMore?: boolean;
  emptyState?: ReactNode;
  className?: string;
}

export function VersionList({
  versions,
  selectedVersionId,
  onSelectVersion,
  onCompare,
  onLoadMore,
  hasMore = false,
  isLoadingMore = false,
  emptyState,
  className,
}: VersionListProps) {
  if (versions.length === 0) {
    if (emptyState) return <>{emptyState}</>;
    return (
      <EmptyState
        icon={History}
        title="No versions found"
        description="Publish a runbook version to start tracking its history."
        className={className}
      />
    );
  }

  return (
    <div data-slot="version-list-container" className={cn('flex flex-col gap-4', className)}>
      <div className="overflow-x-auto border border-border rounded-lg bg-surface">
        <table className="w-full text-left text-xs border-collapse">
          <thead>
            <tr className="border-b border-border bg-surface-raised text-fg-muted font-medium">
              <th scope="col" className="py-2.5 px-4 font-semibold">
                Version
              </th>
              <th scope="col" className="py-2.5 px-4 font-semibold">
                Version ID
              </th>
              <th scope="col" className="py-2.5 px-4 font-semibold">
                Git Reference
              </th>
              <th scope="col" className="py-2.5 px-4 font-semibold">
                Created
              </th>
              <th scope="col" className="py-2.5 px-4 font-semibold text-right">
                Actions
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {versions.map((version, index) => {
              const isSelected = selectedVersionId === version.runbook_version_id;
              const isClickable = !!onSelectVersion;
              const isLatest = index === 0 && version.runbook_version != null;
              const nextVersion = versions[index + 1];

              return (
                <tr
                  key={version.runbook_version_id}
                  data-slot="version-row"
                  onClick={() => onSelectVersion?.(version)}
                  className={cn(
                    'group transition-colors',
                    isSelected && 'bg-accent/10',
                    isClickable && 'cursor-pointer hover:bg-surface-raised',
                  )}
                >
                  {/* Version Number */}
                  <td className="py-3 px-4">
                    <div className="flex items-center gap-2">
                      {isClickable ? (
                        <button
                          type="button"
                          onClick={(e) => {
                            e.stopPropagation();
                            onSelectVersion(version);
                          }}
                          className="text-left font-semibold text-fg hover:text-accent hover:underline focus-visible:outline-2 focus-visible:outline-accent rounded"
                        >
                          {version.runbook_version != null
                            ? `v${version.runbook_version}`
                            : 'preview'}
                        </button>
                      ) : version.runbook_version != null ? (
                        <span className="font-semibold text-fg">v{version.runbook_version}</span>
                      ) : (
                        <span className="inline-flex items-center px-1.5 py-0.5 rounded bg-verdict-warn-bg text-verdict-warn-fg border border-verdict-warn-border text-2xs font-medium">
                          preview
                        </span>
                      )}
                      {isLatest && (
                        <span className="inline-flex items-center px-1.5 py-0.5 rounded bg-accent/10 text-accent text-2xs font-medium">
                          latest
                        </span>
                      )}
                    </div>
                  </td>

                  {/* Version ID */}
                  <td className="py-3 px-4">
                    <MonoRef>{version.runbook_version_id}</MonoRef>
                  </td>

                  {/* Git branch & commit */}
                  <td className="py-3 px-4">
                    {version.branch || version.commit_sha ? (
                      <div className="flex items-center gap-2 text-2xs text-fg-muted">
                        {version.branch && (
                          <span className="inline-flex items-center gap-1">
                            <GitBranch className="size-3 text-fg-muted" aria-hidden="true" />
                            <span className="font-mono text-fg">{version.branch}</span>
                          </span>
                        )}
                        {version.commit_sha && (
                          <span className="inline-flex items-center gap-1 font-mono">
                            <GitCommit className="size-3 text-fg-muted" aria-hidden="true" />
                            <span>{version.commit_sha.slice(0, 7)}</span>
                          </span>
                        )}
                      </div>
                    ) : (
                      <span className="text-fg-muted">—</span>
                    )}
                  </td>

                  {/* Created relative time */}
                  <td className="py-3 px-4 text-fg-muted">
                    {version.created_at_ms ? formatRelativeTime(version.created_at_ms) : '—'}
                  </td>

                  {/* Actions (e.g. Compare with previous) */}
                  <td className="py-3 px-4 text-right">
                    {onCompare && nextVersion && (
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={(e) => {
                          e.stopPropagation();
                          onCompare(version, nextVersion);
                        }}
                        className="h-7 px-2 text-2xs gap-1 text-fg-muted hover:text-fg"
                      >
                        <ArrowLeftRight className="size-3" aria-hidden="true" />
                        Compare
                      </Button>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      {/* Cursor paging: Load more button */}
      {hasMore && onLoadMore && (
        <div className="flex justify-center pt-2">
          <Button
            variant="secondary"
            size="default"
            disabled={isLoadingMore}
            onClick={onLoadMore}
            className="min-w-[120px]"
          >
            {isLoadingMore ? (
              <>
                <Loader2 className="size-3.5 animate-spin mr-1.5" aria-hidden="true" />
                Loading...
              </>
            ) : (
              'Load more'
            )}
          </Button>
        </div>
      )}
    </div>
  );
}
