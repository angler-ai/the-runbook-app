// UI-2: a runbook's own page, graph first. The header says what it is
// (title, slug, description, tags, owners, latest version, whether the
// draft is ahead) with Start run and Edit; the tabs are Graph (default),
// Runs, Versions and Source, addressable as `?tab=`. Graph and Source
// show the latest PUBLISHED version (what a run starts); with nothing
// published they show the draft and say so. `$rb` is a runbook ID or,
// for links that only know it (a `call` node), its slug. Slugs are
// kebab-case (`[a-z0-9]+(-[a-z0-9]+)*`, runbook-model `parse_runbook_ref`),
// so a `$rb` that starts `rb_` is never a slug. An ID from another of the
// person's projects moves to that project's URL; from none, "not found".
import { Link, useNavigate } from '@tanstack/react-router';
import { useQueries, useQuery } from '@tanstack/react-query';
import { Pencil, Play, Users } from 'lucide-react';
import { lazy, Suspense, useEffect, useMemo, useState } from 'react';
import { parse } from 'yaml';
import { api, ApiError } from '../../api/client';
import { runbooksQueryOptions, useMe, useRunbooks } from '../../api/hooks';
import { useOrgContext } from '../context/OrgContext';
import { useCompiler, type CompilerBackend } from '../compile/useCompiler';
import {
  draftQueryOptions,
  editorPermissions,
  runbookQueryOptions,
  type RunbookView,
} from '../editor/hooks';
import { RunbookRuns } from '../executions/RunbookRuns';
import { parseExecutionSearch, toExecutionSearch } from '../executions/filters';
import { VersionsPage } from '../runbooks/VersionsPage';
import { RunbookMemoryTab } from '../memory/RunbookMemoryTab';
import { RunbookArchivesTab } from '../archives/RunbookArchivesTab';
import { StartRunDialog } from '../start/StartRunDialog';
import { EmptyState, ErrorState, PageHeader, Skeleton } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../../ui/primitives/tabs';
import { overviewTab, type OverviewSearch, type OverviewTab } from './tabs';
import { triggersOf } from './triggers';
import { TriggersPanel } from './TriggersPanel';

const RunbookGraph = lazy(() =>
  import('../../ui/graph').then((module) => ({ default: module.RunbookGraph })),
);

const TAB_LABELS: Record<OverviewTab, string> = {
  graph: 'Graph',
  runs: 'Runs',
  versions: 'Versions',
  memory: 'Memory',
  archives: 'Archives',
  source: 'Source',
};

/** `metadata.owners` of a source, as written; [] when absent or unreadable. */
export function ownersOf(source: string | undefined): string[] {
  if (!source) return [];
  try {
    const doc = parse(source) as { metadata?: { owners?: unknown } } | null;
    const owners = doc?.metadata?.owners;
    return Array.isArray(owners) ? owners.filter((o): o is string => typeof o === 'string') : [];
  } catch {
    return [];
  }
}

export interface RunbookOverviewPageProps {
  projectRef: string;
  /** A runbook ID (`rb_…`) or slug. */
  runbookRef: string;
  search: OverviewSearch;
  /** Test seam for the compiler (defaults to the shared Web Worker). */
  compiler?: CompilerBackend;
}

export function RunbookOverviewPage({
  projectRef,
  runbookRef,
  search,
  compiler,
}: RunbookOverviewPageProps) {
  const { resolveProject, isLoading: orgLoading } = useOrgContext();
  const project = resolveProject(projectRef);
  const isId = runbookRef.startsWith('rb_');
  // The project's catalog resolves a slug and confirms an ID is this
  // project's (the runbook read itself does not say which project it is in).
  const catalog = useRunbooks(project?.id);
  const runbookId = isId
    ? runbookRef
    : catalog.data?.find((row) => row.slug === runbookRef)?.runbook_id;

  if (!isId && !runbookId) {
    if (catalog.isLoading || !project) return <OverviewSkeleton />;
    return (
      <EmptyState
        title="No such runbook"
        description={`This project has no runbook called ${runbookRef}.`}
      />
    );
  }
  // Wait for the project and its catalog, so another project's runbook is
  // never drawn under this project's chrome, even briefly; and for a
  // refetch of a cached catalog that lacks it (it may have just been made).
  const listed = catalog.data?.some((r) => r.runbook_id === runbookRef) ?? false;
  if (isId && (orgLoading || (project && (catalog.isLoading || (!listed && catalog.isFetching)))))
    return <OverviewSkeleton />;
  if (isId && project && catalog.data && !listed) {
    return <RunbookElsewhere runbookId={runbookRef} projectId={project.id} search={search} />;
  }
  return (
    <Overview
      projectRef={project?.slug ?? projectRef}
      projectId={project?.id}
      runbookId={runbookId as string}
      search={search}
      compiler={compiler}
    />
  );
}

/**
 * A runbook ID that is not in the route's project: find the person's
 * project that has it (their other projects' catalogs) and move there,
 * keeping the tab; or say it is not here.
 */
function RunbookElsewhere({
  runbookId,
  projectId,
  search,
}: {
  runbookId: string;
  projectId: string;
  search: OverviewSearch;
}) {
  const { projects } = useOrgContext();
  const navigate = useNavigate();
  const others = projects.filter((p) => p.id !== projectId);
  const catalogs = useQueries({ queries: others.map((p) => runbooksQueryOptions(p.id)) });
  const index = catalogs.findIndex((c) => c.data?.some((row) => row.runbook_id === runbookId));
  const owner = index >= 0 ? others[index] : undefined;
  const searching = catalogs.some((c) => c.isLoading);
  useEffect(() => {
    if (owner) {
      void navigate({
        to: '/p/$project/runbooks/$rb',
        params: { project: owner.slug, rb: runbookId },
        search,
        replace: true,
      });
    }
  }, [owner, runbookId, search, navigate]);
  if (owner || searching) return <OverviewSkeleton />;
  return (
    <EmptyState
      headingLevel={1}
      title="No such runbook"
      description="This runbook is not in this project, or in any project you can see."
    />
  );
}

function OverviewSkeleton() {
  return (
    <div aria-busy="true" className="grid gap-4">
      <p role="status" className="sr-only">
        Loading the runbook…
      </p>
      <Skeleton className="h-16 w-2/3" />
      <Skeleton className="h-[420px] w-full" />
    </div>
  );
}

function Overview({
  projectRef,
  projectId,
  runbookId,
  search,
  compiler,
}: {
  projectRef: string;
  projectId: string | undefined;
  runbookId: string;
  search: OverviewSearch;
  compiler?: CompilerBackend;
}) {
  const navigate = useNavigate();
  const me = useMe();
  const { canWrite } = editorPermissions(me.data?.credential.scopes);
  const runbook = useQuery(runbookQueryOptions(runbookId));
  const latest = runbook.data?.latest_version ?? null;
  const published = useQuery({
    queryKey: ['runbooks', runbookId, 'versions', latest == null ? null : String(latest), 'source'],
    enabled: latest != null,
    queryFn: async () => {
      const { data } = await api.GET('/v1/runbooks/{rb}/versions/{version}', {
        params: { path: { rb: runbookId, version: String(latest) } },
      });
      return data as { source: string };
    },
  });
  // Nothing published yet: the draft is all there is to show.
  const draft = useQuery({
    ...draftQueryOptions(runbookId),
    enabled: runbook.isSuccess && latest == null,
  });
  const source = latest != null ? published.data?.source : draft.data?.draft.source;
  // The read the shown source comes from: its failure is an error, never "no source".
  const sourceRead = latest != null ? published : draft;
  const sourceError = sourceRead.isError ? sourceRead.error : null;
  const retrySource = () => void sourceRead.refetch();
  const catalog = useRunbooks(projectId);
  const [startOpen, setStartOpen] = useState(false);
  const tab = overviewTab(search);

  if (runbook.isError) {
    const error = runbook.error;
    return (
      <ErrorState
        title={
          error instanceof ApiError && error.status === 404
            ? 'No such runbook'
            : 'Could not load the runbook'
        }
        code={error instanceof ApiError ? error.code : undefined}
        requestId={error instanceof ApiError ? (error.requestId ?? undefined) : undefined}
        onRetry={() => void runbook.refetch()}
      />
    );
  }
  if (!runbook.data) return <OverviewSkeleton />;
  const row = runbook.data;

  function setTab(next: string) {
    const { tab: _old, run: _run, ...rest } = search;
    void navigate({
      to: '.',
      search: next === 'graph' ? rest : { ...rest, tab: next as OverviewSearch['tab'] },
      replace: true,
    });
  }

  return (
    <>
      <OverviewHeader
        row={row}
        source={source}
        projectRef={projectRef}
        canWrite={canWrite}
        onStart={() => setStartOpen(true)}
      />
      <TriggersPanel
        triggers={triggersOf(source)}
        projectRef={projectRef}
        runbookSlug={row.slug}
        publishedVersion={row.latest_version ?? null}
      />
      <Tabs value={tab} onValueChange={setTab}>
        <TabsList aria-label="Runbook views">
          {(Object.keys(TAB_LABELS) as OverviewTab[]).map((value) => (
            <TabsTrigger key={value} value={value}>
              {TAB_LABELS[value]}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="graph">
          <GraphTab
            source={source}
            error={sourceError}
            onRetry={retrySource}
            loading={latest != null ? published.isLoading : draft.isLoading}
            fromDraft={latest == null}
            compiler={compiler}
            projectRef={projectRef}
            slugToId={new Map((catalog.data ?? []).map((r) => [r.slug, r.runbook_id]))}
          />
        </TabsContent>
        <TabsContent value="runs">
          {tab === 'runs' ? (
            <RunbookRuns
              projectRef={projectRef}
              runbook={row.runbook_id}
              runbookTitle={row.title}
              filters={parseExecutionSearch(search)}
              onFiltersChange={(filters) =>
                void navigate({
                  to: '.',
                  search: { ...toExecutionSearch(filters), tab: 'runs' },
                  replace: true,
                })
              }
              selected={search.run}
              onSelect={(run) =>
                void navigate({ to: '.', search: { ...search, tab: 'runs', run }, replace: true })
              }
            />
          ) : null}
        </TabsContent>
        <TabsContent value="versions">
          {tab === 'versions' ? <VersionsPage runbookId={row.runbook_id} embedded /> : null}
        </TabsContent>
        <TabsContent value="memory">
          {tab === 'memory' ? (
            <RunbookMemoryTab
              runbookId={row.runbook_id}
              projectId={projectId}
              canWrite={canWrite}
            />
          ) : null}
        </TabsContent>
        <TabsContent value="archives">
          {tab === 'archives' ? <RunbookArchivesTab runbookId={row.runbook_id} /> : null}
        </TabsContent>
        <TabsContent value="source">
          <SourceTab
            source={source}
            error={sourceError}
            onRetry={retrySource}
            version={latest}
            loading={latest != null ? published.isLoading : draft.isLoading}
            editHref={`/p/${projectRef}/runbooks/${row.runbook_id}/editor`}
          />
        </TabsContent>
      </Tabs>
      {projectId ? (
        <StartRunDialog
          open={startOpen}
          onOpenChange={setStartOpen}
          projectId={projectId}
          projectRef={projectRef}
          runbooks={catalog.data ?? [row]}
          initialRunbookId={row.runbook_id}
        />
      ) : null}
    </>
  );
}

function OverviewHeader({
  row,
  source,
  projectRef,
  canWrite,
  onStart,
}: {
  row: RunbookView;
  source: string | undefined;
  projectRef: string;
  canWrite: boolean;
  onStart(): void;
}) {
  const owners = useMemo(() => ownersOf(source), [source]);
  const title = row.title || row.slug;
  return (
    <PageHeader
      eyebrow={<span className="normal-case tracking-normal">{row.slug}</span>}
      title={title}
      description={
        <span className="grid gap-2">
          {row.description ? (
            <span>
              {row.description}
              {row.description_truncated ? '…' : null}
            </span>
          ) : null}
          <span className="flex flex-wrap items-center gap-x-3 gap-y-1.5 text-xs">
            {row.latest_version != null ? (
              <span className="rounded border border-border px-1.5 font-mono text-2xs leading-5 text-fg">
                v{row.latest_version}
              </span>
            ) : (
              <span className="rounded border border-dashed border-border-strong px-1.5 font-mono text-2xs leading-5 text-fg-muted">
                unpublished
              </span>
            )}
            {row.draft_ahead ? (
              <span
                className="inline-flex items-center gap-1 font-mono text-2xs text-warning-text"
                title="The draft has changes that are not published"
              >
                <span aria-hidden="true" className="size-1.5 rounded-full bg-warning" />
                draft ahead
              </span>
            ) : null}
            {owners.length > 0 ? (
              <span className="inline-flex items-center gap-1 text-fg-muted">
                <Users className="size-3.5" aria-hidden="true" />
                <span className="sr-only">Owners: </span>
                {owners.join(', ')}
              </span>
            ) : null}
            {(row.tags ?? []).length > 0 ? (
              <ul aria-label="Tags" className="flex flex-wrap gap-1">
                {(row.tags ?? []).map((tag) => (
                  <li
                    key={tag}
                    className="rounded bg-surface-raised px-1.5 font-mono text-[11px] leading-5 text-fg-muted ring-1 ring-border ring-inset"
                  >
                    {tag}
                  </li>
                ))}
              </ul>
            ) : null}
          </span>
        </span>
      }
      actions={
        <>
          <Button
            onClick={onStart}
            disabled={row.latest_version == null}
            title={row.latest_version == null ? 'Publish a version first' : undefined}
          >
            <Play aria-hidden="true" />
            Start run
          </Button>
          <Button variant="secondary" asChild>
            <Link
              to="/p/$project/runbooks/$rb/editor"
              params={{ project: projectRef, rb: row.runbook_id }}
            >
              <Pencil aria-hidden="true" />
              {canWrite ? 'Edit' : 'Open editor'}
            </Link>
          </Button>
        </>
      }
    />
  );
}

function SourceError({ error, onRetry }: { error: unknown; onRetry(): void }) {
  return (
    <ErrorState
      title="The source could not be loaded"
      message="The graph and the source need it. Retry, or open the editor."
      code={error instanceof ApiError ? error.code : undefined}
      requestId={error instanceof ApiError ? (error.requestId ?? undefined) : undefined}
      onRetry={onRetry}
    />
  );
}

function GraphTab({
  source,
  error,
  onRetry,
  loading,
  fromDraft,
  compiler,
  projectRef,
  slugToId,
}: {
  source: string | undefined;
  error: unknown;
  onRetry(): void;
  loading: boolean;
  fromDraft: boolean;
  compiler?: CompilerBackend;
  projectRef: string;
  slugToId: ReadonlyMap<string, string>;
}) {
  const navigate = useNavigate();
  const compiled = useCompiler(source ?? '', { enabled: Boolean(source), backend: compiler });
  const outline = compiled.outline?.outline ?? null;
  if (error) return <SourceError error={error} onRetry={onRetry} />;
  if (
    loading ||
    (source && !outline && (compiled.pending || compiled.outline === null) && !compiled.error)
  ) {
    return <Skeleton className="h-[min(70vh,640px)] w-full" />;
  }
  if (compiled.error) {
    return <ErrorState title="The graph could not be drawn" message={compiled.error} />;
  }
  if (!outline) {
    return (
      <EmptyState
        title="Nothing to draw yet"
        description={
          source
            ? 'The source does not parse; open the editor to fix it.'
            : 'This runbook has no source yet.'
        }
      />
    );
  }
  return (
    <div className="grid gap-2">
      {fromDraft ? (
        <p role="note" className="text-xs text-fg-muted">
          Nothing is published yet: this is the draft.
        </p>
      ) : null}
      <Suspense fallback={<Skeleton className="h-[min(70vh,640px)] w-full" />}>
        <RunbookGraph
          outline={outline}
          className="h-[min(72vh,680px)]"
          fillViewport
          callHref={(slug: string) => `/p/${projectRef}/runbooks/${slugToId.get(slug) ?? slug}`}
          onNavigate={(href) => void navigate({ href })}
          viewKey={`runbook:${projectRef}:${outline.id}`}
        />
      </Suspense>
    </div>
  );
}

function SourceTab({
  source,
  error,
  onRetry,
  version,
  loading,
  editHref,
}: {
  source: string | undefined;
  error: unknown;
  onRetry(): void;
  version: number | null;
  loading: boolean;
  editHref: string;
}) {
  if (error) return <SourceError error={error} onRetry={onRetry} />;
  if (loading) return <Skeleton className="h-64 w-full" />;
  return (
    <div className="grid gap-2">
      <div className="flex items-center justify-between gap-3">
        <p className="text-xs text-fg-muted">
          {version != null
            ? `Published version ${version}, read-only.`
            : 'The draft (nothing is published yet), read-only.'}
        </p>
        <Link
          to={editHref}
          className="text-xs font-medium text-accent hover:text-accent-strong hover:underline"
        >
          Edit draft
        </Link>
      </div>
      <pre
        aria-label={version != null ? `Source of version ${version}` : 'Draft source'}
        // A scrollable region must take keyboard focus to scroll (WCAG 2.1.1).
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
        tabIndex={0}
        className="max-h-[70vh] overflow-auto rounded-lg border border-border bg-surface-raised p-4 font-mono text-xs leading-relaxed text-fg focus-visible:outline-2 focus-visible:outline-focus"
      >
        {source ?? ''}
      </pre>
    </div>
  );
}
