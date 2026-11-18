// UI-2: the runbook page's tabs, addressable in the URL (`?tab=`). Graph is
// the default and leaves the URL clean; the Runs tab keeps the execution
// list's own filters (`status`, `tags`) beside it.
import { validateExecutionSearch, type ExecutionSearch } from '../executions/filters';

export const OVERVIEW_TABS = ['graph', 'runs', 'versions', 'memory', 'archives', 'source'] as const;
export type OverviewTab = (typeof OVERVIEW_TABS)[number];

export interface OverviewSearch extends ExecutionSearch {
  tab?: Exclude<OverviewTab, 'graph'>;
  /** The run picked in the Runs tab. */
  run?: string;
}

export function isOverviewTab(value: unknown): value is OverviewTab {
  return typeof value === 'string' && (OVERVIEW_TABS as readonly string[]).includes(value);
}

export function validateOverviewSearch(search: Record<string, unknown>): OverviewSearch {
  const { runbook: _locked, ...filters } = validateExecutionSearch(search);
  const tab = search.tab;
  return {
    ...filters,
    ...(isOverviewTab(tab) && tab !== 'graph' ? { tab } : {}),
    ...(typeof search.run === 'string' && search.run ? { run: search.run } : {}),
  };
}

export function overviewTab(search: OverviewSearch): OverviewTab {
  return search.tab ?? 'graph';
}

// The editor's tab: Graph unless the person last chose another there.
export const EDITOR_TABS = ['graph', 'yaml', 'outline'] as const;
export type EditorTab = (typeof EDITOR_TABS)[number];
const EDITOR_TAB_KEY = 'rb.editor.tab';

export function lastEditorTab(): EditorTab {
  try {
    const saved = localStorage.getItem(EDITOR_TAB_KEY);
    if (saved && (EDITOR_TABS as readonly string[]).includes(saved)) return saved as EditorTab;
  } catch {
    // Storage blocked: the default.
  }
  return 'graph';
}

export function rememberEditorTab(tab: string): void {
  if (!(EDITOR_TABS as readonly string[]).includes(tab)) return;
  try {
    localStorage.setItem(EDITOR_TAB_KEY, tab);
  } catch {
    // Storage blocked: nothing to remember.
  }
}
