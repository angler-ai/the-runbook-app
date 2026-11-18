// Route page wiring (M1-M3): thin adapters from route params/search to
// the feature pages. The editor (M5) is lazily loaded from ./editor.
import { Navigate, useNavigate, useParams, useSearch } from '@tanstack/react-router';
import { useState } from 'react';
import { useMe } from '../api/hooks';
import { useOrgContext } from '../features/context/OrgContext';
import { pickProject, useLastProject } from '../features/context/currentProject';
import { HomePage } from '../features/home/HomePage';
import { RunbookRuns } from '../features/executions/RunbookRuns';
import { ExecutionList } from '../features/executions/ExecutionList';
import {
  parseExecutionSearch,
  toExecutionSearch,
  type ExecutionSearch,
} from '../features/executions/filters';
import { ExecutionDocumentPage } from '../features/execution/ExecutionDocumentPage';
import { defaultRunView, NARROW_RUN_QUERY, type RunSearch } from '../features/execution/views';
import { TasksPage } from '../features/inbox/TasksPage';
import { RunbooksPage } from '../features/runbooks/RunbooksPage';
import { VersionsPage } from '../features/runbooks/VersionsPage';
import { useResolvedRunbookId } from '../features/runbooks/hooks';
import { EmptyState, ErrorState, PageHeader, Skeleton } from '../ui/layout';

export { TasksPage };

/** UI-2: `/home`, where sign-in lands: the current project's Home, or the inbox without one. */
export function HomeLanding() {
  const { org, projects, isLoading } = useOrgContext();
  const me = useMe();
  const remembered = useLastProject(org?.id ?? me.data?.org.id);
  const project = pickProject({ projects, route: undefined, execution: undefined, remembered });
  if (project) return <Navigate to="/p/$project" params={{ project: project.slug }} replace />;
  if (!isLoading && !me.isLoading) return <Navigate to="/tasks" replace />;
  return (
    <div aria-busy="true" className="grid gap-3">
      <p role="status" className="sr-only">
        Loading Home…
      </p>
      <Skeleton className="h-28 w-full" />
    </div>
  );
}

export function ProjectHomePage() {
  const { project } = useParams({ strict: false }) as { project: string };
  return <HomePage projectRef={project} />;
}

export function ProjectRunbooksPage() {
  const { project } = useParams({ strict: false }) as { project: string };
  return <RunbooksPage projectRef={project} />;
}

function useExecutionFilters() {
  const search = useSearch({ strict: false }) as ExecutionSearch;
  const navigate = useNavigate();
  const filters = parseExecutionSearch(search);
  return {
    filters,
    onFiltersChange: (next: typeof filters) =>
      void navigate({ to: '.', search: toExecutionSearch(next), replace: true }),
  };
}

export function ProjectExecutionsPage() {
  const { project } = useParams({ strict: false }) as { project: string };
  const { filters, onFiltersChange } = useExecutionFilters();
  return (
    <>
      <PageHeader title="Runs" />
      <ExecutionList projectRef={project} filters={filters} onFiltersChange={onFiltersChange} />
    </>
  );
}

export function RunbookRunsPage() {
  const { project, rb } = useParams({ strict: false }) as { project: string; rb: string };
  const { filters, onFiltersChange } = useExecutionFilters();
  const [run, setRun] = useState<string | undefined>(undefined);
  return (
    <>
      <PageHeader title="Runs" />
      <RunbookRuns
        projectRef={project}
        runbook={rb}
        filters={filters}
        onFiltersChange={(next) => {
          setRun(undefined);
          onFiltersChange(next);
        }}
        selected={run}
        onSelect={setRun}
      />
    </>
  );
}

export function RunbookVersionsPage() {
  const { project, rb } = useParams({ strict: false }) as { project: string; rb: string };
  const { resolveProject } = useOrgContext();
  const resolved = useResolvedRunbookId(resolveProject(project)?.id, rb);
  if (resolved.pending) {
    return (
      <div aria-busy="true">
        <p role="status" className="sr-only">
          Loading versions…
        </p>
        <Skeleton className="h-24 w-full" />
      </div>
    );
  }
  if (resolved.failed) {
    return <ErrorState message="This runbook could not be looked up." onRetry={resolved.retry} />;
  }
  if (resolved.missing || !resolved.runbookId) {
    return (
      <EmptyState
        title="Runbook not found"
        description="No runbook has that slug in this project."
      />
    );
  }
  return <VersionsPage runbookId={resolved.runbookId} />;
}

export function ExecutionPage() {
  const { exe } = useParams({ strict: false }) as { exe: string };
  const search = useSearch({ strict: false }) as RunSearch;
  const navigate = useNavigate();
  // Read once: a run opens on Steps on a phone and on Graph elsewhere; the
  // URL keeps any other choice.
  const [fallback] = useState(() =>
    defaultRunView(globalThis.matchMedia?.(NARROW_RUN_QUERY).matches ?? false),
  );
  return (
    <ExecutionDocumentPage
      executionId={exe}
      view={search.view ?? fallback}
      onViewChange={(view) =>
        void navigate({ to: '.', search: view === fallback ? {} : { view }, replace: true })
      }
    />
  );
}
