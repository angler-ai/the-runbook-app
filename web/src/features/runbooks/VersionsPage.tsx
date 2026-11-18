// Versions page (M3): cursor-paged version list; compare any two
// versions (selects) or adjacent versions (the list's Compare action)
// through the VersionDiffView route, with both sources for DiffView.
import { useState } from 'react';
import { ApiError } from '../../api/client';
import { ErrorState, PageHeader, Skeleton } from '../../ui/layout';
import { DiffView, VersionList } from '../../ui/lists';
import { Button } from '../../ui/primitives/button';
import { Label } from '../../ui/primitives/label';
import { useVersionDiff, useVersionSource, useVersions, versionRef } from './hooks';

/**
 * `embedded`: shown inside another page (the runbook page's Versions tab),
 * whose header is the page's one h1, so this adds none of its own.
 */
export function VersionsPage({
  runbookId,
  embedded = false,
}: {
  runbookId: string;
  embedded?: boolean;
}) {
  const versionsQuery = useVersions(runbookId);
  const versions = (versionsQuery.data?.pages ?? []).flatMap((page) => page.items);
  const [from, setFrom] = useState<string>('');
  const [to, setTo] = useState<string>('');
  const [comparing, setComparing] = useState<{ a: string; b: string } | null>(null);

  const diff = useVersionDiff(runbookId, comparing?.a ?? null, comparing?.b ?? null);
  const sourceA = useVersionSource(runbookId, comparing?.a ?? null);
  const sourceB = useVersionSource(runbookId, comparing?.b ?? null);

  const options = versions.map((version) => ({
    ref: versionRef(version),
    label:
      version.runbook_version != null
        ? `v${version.runbook_version}`
        : `preview ${version.runbook_version_id}`,
  }));
  const fromValue = from || options[1]?.ref || options[0]?.ref || '';
  const toValue = to || options[0]?.ref || '';

  return (
    <>
      {embedded ? null : <PageHeader title="Versions" />}
      {versionsQuery.isLoading ? (
        <div aria-busy="true">
          <p role="status" className="sr-only">
            Loading versions…
          </p>
          <Skeleton className="h-24 w-full" />
        </div>
      ) : null}
      {versionsQuery.isError ? (
        <ErrorState
          message="Versions could not be loaded."
          code={versionsQuery.error instanceof ApiError ? versionsQuery.error.code : undefined}
          requestId={
            versionsQuery.error instanceof ApiError
              ? (versionsQuery.error.requestId ?? undefined)
              : undefined
          }
          onRetry={() => void versionsQuery.refetch()}
        />
      ) : null}
      {versionsQuery.data ? (
        <VersionList
          versions={versions}
          hasMore={Boolean(versionsQuery.hasNextPage)}
          isLoadingMore={versionsQuery.isFetchingNextPage}
          onLoadMore={() => void versionsQuery.fetchNextPage()}
          onCompare={(v1, v2) => setComparing({ a: versionRef(v2), b: versionRef(v1) })}
        />
      ) : null}
      {options.length >= 2 ? (
        <form
          className="mt-6 flex flex-wrap items-end gap-3"
          onSubmit={(event) => {
            event.preventDefault();
            if (fromValue && toValue && fromValue !== toValue) {
              setComparing({ a: fromValue, b: toValue });
            }
          }}
        >
          <div className="grid gap-1">
            <Label htmlFor="diff-from">Compare from</Label>
            <select id="diff-from" value={fromValue} onChange={(e) => setFrom(e.target.value)}>
              {options.map((option) => (
                <option key={option.ref} value={option.ref}>
                  {option.label}
                </option>
              ))}
            </select>
          </div>
          <div className="grid gap-1">
            <Label htmlFor="diff-to">Compare to</Label>
            <select id="diff-to" value={toValue} onChange={(e) => setTo(e.target.value)}>
              {options.map((option) => (
                <option key={option.ref} value={option.ref}>
                  {option.label}
                </option>
              ))}
            </select>
          </div>
          <Button type="submit" variant="secondary">
            Compare versions
          </Button>
        </form>
      ) : null}
      {comparing ? (
        <section aria-label={`Diff ${comparing.a} to ${comparing.b}`} className="mt-4 grid gap-3">
          <h2 className="font-semibold">
            Changes from {comparing.a} to {comparing.b}
          </h2>
          {diff.isLoading || sourceA.isLoading || sourceB.isLoading ? (
            <p role="status">Loading diff…</p>
          ) : null}
          {diff.isError ? <p role="alert">The diff could not be loaded.</p> : null}
          {diff.data ? (
            <DiffView diff={diff.data} before={sourceA.data?.source} after={sourceB.data?.source} />
          ) : null}
        </section>
      ) : null}
    </>
  );
}
