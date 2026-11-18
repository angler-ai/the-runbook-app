// UI-2: the current project, kept across pages that have no project in
// their URL (/tasks, /x/$exe). The route's project wins, then the run's
// own project, then the project this person last chose in this
// organization (localStorage, per org), and only then the first project.
import { useSyncExternalStore } from 'react';
import type { ProjectView } from '../../api/hooks';

const key = (orgId: string) => `rb.project.${orgId}`;
const listeners = new Set<() => void>();

/** The slug last chosen in `orgId`, or null when unknown or storage is unavailable. */
export function lastProject(orgId: string | undefined): string | null {
  if (!orgId) return null;
  try {
    return localStorage.getItem(key(orgId));
  } catch {
    return null;
  }
}

/** Remember `slug` as `orgId`'s current project. Storage failures are ignored. */
export function rememberProject(orgId: string | undefined, slug: string): void {
  if (!orgId) return;
  try {
    if (localStorage.getItem(key(orgId)) === slug) return;
    localStorage.setItem(key(orgId), slug);
  } catch {
    return;
  }
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The remembered slug, re-read when this tab remembers another. */
export function useLastProject(orgId: string | undefined): string | null {
  return useSyncExternalStore(
    subscribe,
    () => lastProject(orgId),
    () => null,
  );
}

function find(projects: ProjectView[], ref: string | null | undefined) {
  return ref ? projects.find((p) => p.id === ref || p.slug === ref) : undefined;
}

export function pickProject({
  projects,
  route,
  execution,
  remembered,
}: {
  projects: ProjectView[];
  /** The `$project` route parameter (slug or ID). */
  route: string | undefined;
  /** The project ID of the run an execution page shows. */
  execution: string | null | undefined;
  remembered: string | null;
}): ProjectView | undefined {
  return (
    find(projects, route) ?? find(projects, execution) ?? find(projects, remembered) ?? projects[0]
  );
}
