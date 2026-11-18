// UI-3: the run page's views, addressable in the URL (`?view=`). Graph is
// the default and leaves the URL clean; Document reads the run as a page;
// Steps is the step-by-step list with inline actions (M2's page).
export const RUN_VIEWS = ['graph', 'document', 'steps', 'checklist'] as const;
export type RunView = (typeof RUN_VIEWS)[number];

export interface RunSearch {
  view?: RunView;
}

/** Below this width the graph is hard to use, so a run opens on Steps. */
export const NARROW_RUN_QUERY = '(max-width: 639px)';

/** The view a run opens on when the URL names none: Graph, or Steps on a phone. */
export function defaultRunView(narrow: boolean): RunView {
  return narrow ? 'steps' : 'graph';
}

export const RUN_VIEW_LABELS: Record<RunView, string> = {
  graph: 'Graph',
  document: 'Document',
  steps: 'Steps',
  checklist: 'Checklist',
};

export function isRunView(value: unknown): value is RunView {
  return typeof value === 'string' && (RUN_VIEWS as readonly string[]).includes(value);
}

export function validateRunSearch(search: Record<string, unknown>): RunSearch {
  const view = search.view;
  // `graph` stays in the URL: on a phone it is a choice, not the default.
  return isRunView(view) ? { view } : {};
}
