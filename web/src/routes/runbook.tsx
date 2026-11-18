// The runbook overview route (UI-2), loaded lazily with the graph and the
// browser compiler.
import { useParams, useSearch } from '@tanstack/react-router';
import { RunbookOverviewPage } from '../features/runbook/RunbookOverviewPage';
import type { OverviewSearch } from '../features/runbook/tabs';

export function RunbookOverviewRoute() {
  const { project, rb } = useParams({ strict: false }) as { project: string; rb: string };
  const search = useSearch({ strict: false }) as OverviewSearch;
  return <RunbookOverviewPage projectRef={project} runbookRef={rb} search={search} />;
}
