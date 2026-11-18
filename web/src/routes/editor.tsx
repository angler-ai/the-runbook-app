// The editor route's component (M5), loaded lazily by router.tsx so Monaco,
// the graph and the editor stay out of the main bundle.
import { useParams, useSearch } from '@tanstack/react-router';
import type { NewRunbookSearch } from '../features/builder/templates';
import { RunbookEditorPage } from '../features/editor/RunbookEditorPage';
import { useOrgContext } from '../features/context/OrgContext';
import { useResolvedRunbookId } from '../features/runbooks/hooks';
import { EmptyState, ErrorState, Skeleton } from '../ui/layout';

export function RunbookEditorRoute() {
  const { project, rb } = useParams({ strict: false }) as { project: string; rb: string };
  const { resolveProject } = useOrgContext();
  const resolved = useResolvedRunbookId(resolveProject(project)?.id, rb);
  if (resolved.pending) {
    return (
      <div aria-busy="true">
        <p role="status" className="sr-only">
          Loading the editor…
        </p>
        <Skeleton className="h-64 w-full" />
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
  return (
    <RunbookEditorPage
      key={resolved.runbookId}
      projectRef={project}
      runbookId={resolved.runbookId}
    />
  );
}

/** UI-4: `/p/$project/runbooks/new`, a new runbook in the builder (unsaved). */
export function NewRunbookRoute() {
  const { project } = useParams({ strict: false }) as { project: string };
  const start = useSearch({ strict: false }) as NewRunbookSearch;
  return <RunbookEditorPage key={`new:${project}`} projectRef={project} start={start} />;
}
