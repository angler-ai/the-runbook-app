// UI-1: the runbooks list's model. The server searches (`q`) and filters by
// tag (`tag`); last-run status filtering and sorting are done here, over the
// rows loaded so far, and the page says so when more rows exist.
import type { Schemas } from '../../api/client';

export type ExecutionStatus = Schemas['ExecutionStatus'];

export type LastRun = Schemas['RunbookLastRun'];

/**
 * A row of GET /v1/projects/{p}/runbooks. `last_run` and `active_runs` are
 * omitted unless the caller also holds executions:read on the project (a
 * server that may send them always sends `active_runs`, even 0), so an absent
 * `active_runs` means "run data withheld", not "nothing running".
 * `description_truncated` is true when the server cut `description` short.
 */
export type RunbookRow = Schemas['RunbookCatalogView'];

/** Trimmed, non-empty and each once (the server does this too; defensive). */
export function uniqueTags(tags: readonly string[] | null | undefined): string[] {
  return [...new Set((tags ?? []).map((tag) => tag.trim()).filter(Boolean))];
}

/**
 * A row as the server sent it, with the UI-1 additions defaulted: an older
 * server omits tags and draft_ahead. Run data is left as sent (see RunbookRow).
 */
export function normalizeRow(
  row: Omit<RunbookRow, 'tags' | 'draft_ahead'> & Partial<Pick<RunbookRow, 'tags' | 'draft_ahead'>>,
): RunbookRow {
  return {
    ...row,
    tags: uniqueTags(row.tags),
    draft_ahead: row.draft_ahead ?? false,
  };
}

/** Whether this row carries run data (the caller may read executions). */
export function rowHasRunData(row: RunbookRow): boolean {
  return row.active_runs != null || row.last_run != null;
}

/** Whether any loaded row carries run data: the run-based filters and sorts need it. */
export function hasRunData(rows: readonly RunbookRow[]): boolean {
  return rows.some(rowHasRunData);
}

/**
 * The `tag` query parameter: tags joined with commas, a literal comma written
 * `\,` and a literal backslash `\\` (the server unescapes).
 */
export function encodeTagParam(tags: readonly string[]): string {
  return tags.map((tag) => tag.replace(/\\/g, '\\\\').replace(/,/g, '\\,')).join(',');
}

/** What a runbook's last run says about it, for filtering at a glance. */
export type RunHealth = 'failing' | 'attention' | 'running' | 'passing' | 'never' | 'other';

export function runHealth(row: RunbookRow): RunHealth {
  const status = row.last_run?.status;
  switch (status) {
    case undefined:
      return 'never';
    case 'failed':
      return 'failing';
    case 'waiting':
    case 'paused':
      return 'attention';
    case 'queued':
    case 'running':
      return 'running';
    case 'succeeded':
      return 'passing';
    default:
      return 'other';
  }
}

export type HealthFilter = 'all' | Exclude<RunHealth, 'other'>;

export const HEALTH_FILTERS: { value: HealthFilter; label: string }[] = [
  { value: 'all', label: 'All' },
  { value: 'failing', label: 'Failing' },
  { value: 'attention', label: 'Needs a person' },
  { value: 'running', label: 'Running' },
  { value: 'passing', label: 'Passing' },
  { value: 'never', label: 'Never run' },
];

export function healthCounts(rows: RunbookRow[]): Record<HealthFilter, number> {
  const counts: Record<HealthFilter, number> = {
    all: rows.length,
    failing: 0,
    attention: 0,
    running: 0,
    passing: 0,
    never: 0,
  };
  for (const row of rows) {
    const health = runHealth(row);
    if (health !== 'other') counts[health] += 1;
  }
  return counts;
}

export function filterByHealth(rows: RunbookRow[], filter: HealthFilter): RunbookRow[] {
  return filter === 'all' ? rows : rows.filter((row) => runHealth(row) === filter);
}

export type SortKey = 'recent' | 'name' | 'last-run' | 'attention';

/** Sorts that read run data, offered only when the rows carry it. */
export const RUN_SORTS: ReadonlySet<SortKey> = new Set<SortKey>(['attention', 'last-run']);

export const SORTS: { value: SortKey; label: string }[] = [
  { value: 'recent', label: 'Newest first' },
  { value: 'attention', label: 'Needs attention first' },
  { value: 'last-run', label: 'Last run' },
  { value: 'name', label: 'Name' },
];

const ATTENTION_ORDER: Record<RunHealth, number> = {
  failing: 0,
  attention: 1,
  running: 2,
  other: 3,
  passing: 4,
  never: 5,
};

export function displayTitle(row: RunbookRow): string {
  return row.title || row.slug;
}

/** `recent` keeps the server's order (newest runbook first). */
export function sortRows(rows: RunbookRow[], sort: SortKey): RunbookRow[] {
  if (sort === 'recent') return rows;
  const copy = [...rows];
  const byName = (a: RunbookRow, b: RunbookRow) =>
    displayTitle(a).localeCompare(displayTitle(b), undefined, { sensitivity: 'base' });
  const lastRunAt = (row: RunbookRow) => row.last_run?.created_at_ms ?? -Infinity;
  if (sort === 'name') return copy.sort(byName);
  if (sort === 'last-run') return copy.sort((a, b) => lastRunAt(b) - lastRunAt(a) || byName(a, b));
  return copy.sort(
    (a, b) =>
      ATTENTION_ORDER[runHealth(a)] - ATTENTION_ORDER[runHealth(b)] ||
      lastRunAt(b) - lastRunAt(a) ||
      byName(a, b),
  );
}

/** Every tag on the loaded rows, most used first. */
export function collectTags(rows: RunbookRow[]): string[] {
  const counts = new Map<string, number>();
  for (const row of rows)
    for (const tag of uniqueTags(row.tags)) counts.set(tag, (counts.get(tag) ?? 0) + 1);
  return [...counts.entries()]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([tag]) => tag);
}

/** A slug from a title: lowercase letters, digits and single hyphens. */
export function slugify(title: string): string {
  return title
    .normalize('NFKD')
    .replace(/\p{M}/gu, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 63)
    .replace(/-+$/g, '');
}
