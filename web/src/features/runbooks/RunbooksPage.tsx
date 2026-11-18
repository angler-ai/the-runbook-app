// Runbooks list (UI-1; M3, M5 before it). Built for a team whose runbooks
// are mostly agentic ops checks: each row leads with the last run's result
// and anything running now. Search and tag filters run on the server; the
// last-run filter and sorting run over the loaded rows, and the page says so
// while more rows exist. Cursor paging loads 25 at a time. A caller without
// executions:read gets no run data, so the run-based filter and sorts hide.
import { Link, useNavigate } from '@tanstack/react-router';
import { ChevronDown, Search, Tag, X } from 'lucide-react';
import { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react';
import { ApiError } from '../../api/client';
import { useMe } from '../../api/hooks';
import { useOrgContext } from '../context/OrgContext';
import { CreateRunbookDialog } from '../editor/CreateRunbookDialog';
import { editorPermissions } from '../editor/hooks';
import { StartRunDialog } from '../start/StartRunDialog';
import { ErrorState, PageHeader, Skeleton } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import { cn } from '../../ui/primitives/cn';
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '../../ui/primitives/dropdown-menu';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '../../ui/primitives/select';
import {
  HEALTH_FILTERS,
  RUN_SORTS,
  SORTS,
  collectTags,
  filterByHealth,
  hasRunData,
  healthCounts,
  sortRows,
  type HealthFilter,
  type SortKey,
} from './catalog';
import { useRunbookCatalog } from './hooks';
import { RunbookList, type RunbookLinks } from './RunbookList';
import { RunbooksEmpty } from './RunbooksEmpty';

/** `value`, settled for `ms`; `flush` settles it now. */
function useDebounced<T>(value: T, ms: number): [T, () => void] {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  const flush = useCallback(() => setDebounced(value), [value]);
  return [debounced, flush];
}

/**
 * Where "/" belongs to what has focus: fields, dialogs, and open menus,
 * lists and popovers (their typeahead reads it).
 */
const SLASH_OWNERS = [
  'input',
  'textarea',
  'select',
  '[contenteditable="true"]',
  '[role="dialog"]',
  '[role="alertdialog"]',
  '[role="menu"]',
  '[role="listbox"]',
  '[role="combobox"]',
  '[data-radix-popper-content-wrapper]',
].join(', ');

function ownsSlash(node: EventTarget | null): boolean {
  return node instanceof Element && node.closest(SLASH_OWNERS) !== null;
}

function ListSkeleton() {
  return (
    <div aria-busy="true" className="overflow-hidden rounded-lg border border-border bg-surface">
      <p role="status" className="sr-only">
        Loading runbooks…
      </p>
      {Array.from({ length: 5 }, (_, i) => (
        <div key={i} className="grid gap-2 border-b border-border px-4 py-4 last:border-0">
          <Skeleton className="h-4 w-1/3" />
          <Skeleton className="h-3 w-2/3" />
        </div>
      ))}
    </div>
  );
}

export function RunbooksPage({ projectRef }: { projectRef: string }) {
  const navigate = useNavigate();
  const { resolveProject } = useOrgContext();
  const project = resolveProject(projectRef);
  const { canWrite } = editorPermissions(useMe().data?.credential.scopes);

  const [search, setSearch] = useState('');
  const [q, flushQ] = useDebounced(search.trim(), 250);
  const [tags, setTags] = useState<string[]>([]);
  const [health, setHealth] = useState<HealthFilter>('all');
  const [sort, setSort] = useState<SortKey>('recent');
  const searchRef = useRef<HTMLInputElement>(null);
  const countsNoteId = useId();

  const catalog = useRunbookCatalog(project?.id, { q, tags });
  const loaded = useMemo(
    () => (catalog.data?.pages ?? []).flatMap((page) => page.items),
    [catalog.data],
  );
  // Run data (last run, active runs) is withheld from a caller without
  // executions:read; the last-run filter and the run-based sorts need it.
  const runData = hasRunData(loaded);
  const effectiveHealth: HealthFilter = runData ? health : 'all';
  const effectiveSort: SortKey = runData || !RUN_SORTS.has(sort) ? sort : 'recent';
  const sortOptions = runData ? SORTS : SORTS.filter((option) => !RUN_SORTS.has(option.value));
  const counts = useMemo(() => healthCounts(loaded), [loaded]);
  const visible = useMemo(
    () => sortRows(filterByHealth(loaded, effectiveHealth), effectiveSort),
    [loaded, effectiveHealth, effectiveSort],
  );
  // Tags offered: the loaded rows' tags, plus any chosen (they may filter
  // every other row out).
  const tagOptions = useMemo(() => [...new Set([...tags, ...collectTags(loaded)])], [loaded, tags]);

  const [startOpen, setStartOpen] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [startRunbookId, setStartRunbookId] = useState<string | undefined>(undefined);

  // "/" focuses search, as in Shelf's palette-first pages: not from a field,
  // dialog or open menu (whose typeahead reads it), nor once handled.
  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (event.key !== '/' || event.defaultPrevented) return;
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      if (ownsSlash(event.target) || ownsSlash(document.activeElement)) return;
      event.preventDefault();
      searchRef.current?.focus();
    }
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);

  const filtered = q !== '' || tags.length > 0;
  const localFilter = effectiveHealth !== 'all' || effectiveSort !== 'recent';
  const hasMore = Boolean(catalog.hasNextPage);
  // A real answer, not the previous search's rows kept on screen while a new
  // one loads (keepPreviousData): those may be none, yet say nothing of the
  // project.
  const answered = Boolean(catalog.data) && !catalog.isPlaceholderData;
  const emptyProject = answered && loaded.length === 0 && !filtered;
  const countLabel = catalog.data ? `${loaded.length}${hasMore ? '+' : ''}` : undefined;

  // The toolbar appears with the first answer (rows or an error) and then
  // stays through failed and loading searches, so the person can change or
  // clear what they typed without losing focus.
  const [settledFor, setSettledFor] = useState<string | undefined>(undefined);
  if (project && settledFor !== project.id && (answered || catalog.isError))
    setSettledFor(project.id);
  const settled = project !== undefined && settledFor === project.id;

  /** Retry what the search box says now (the debounced text may lag it). */
  function retry() {
    if (search.trim() !== q) flushQ();
    else void catalog.refetch();
  }

  const links: RunbookLinks = {
    open: (row) => `/p/${projectRef}/runbooks/${row.runbook_id}`,
    edit: (row) => `/p/${projectRef}/runbooks/${row.runbook_id}/editor`,
    versions: (row) => `/p/${projectRef}/runbooks/${row.runbook_id}/versions`,
    run: (id) => `/x/${id}`,
  };

  function toggleTag(tag: string, checked: boolean) {
    setTags((current) => {
      const others = current.filter((t) => t !== tag);
      return checked ? [...others, tag] : others;
    });
  }

  function clearFilters() {
    setSearch('');
    setTags([]);
    setHealth('all');
  }

  let loadedNote: string | null = null;
  if (hasMore && runData)
    loadedNote = `Counts cover the ${loaded.length} runbooks loaded so far${
      localFilter ? ', and so do the last-run filter and sorting' : ''
    }. Load more to include the rest.`;
  else if (hasMore && localFilter)
    loadedNote = `Sorting applies to the ${loaded.length} runbooks loaded so far. Load more to include the rest.`;

  return (
    <>
      <PageHeader
        eyebrow={project?.title}
        title="Runbooks"
        count={countLabel}
        description="Checks and procedures your agents and people run, with how each last went."
        actions={
          <>
            <Button
              variant="secondary"
              onClick={() => {
                setStartRunbookId(loaded.find((r) => r.latest_version != null)?.runbook_id);
                setStartOpen(true);
              }}
            >
              Start a run
            </Button>
            {canWrite ? <Button onClick={() => setCreateOpen(true)}>New runbook</Button> : null}
          </>
        }
      />

      {!project || !settled ? (
        <ListSkeleton />
      ) : emptyProject ? (
        <RunbooksEmpty
          projectId={project.id}
          projectRef={projectRef}
          projectTitle={project.title}
          canWrite={canWrite}
        />
      ) : (
        <div className="grid gap-3">
          {/* Toolbar: search (server), tags (server), sort (loaded rows) */}
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
            <div className="relative min-w-0 flex-1">
              <Search
                className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-fg-muted"
                aria-hidden="true"
              />
              <input
                ref={searchRef}
                type="search"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                aria-label="Search runbooks"
                placeholder="Search title, slug, description or tag"
                className="h-11 w-full rounded-md border border-border-input bg-surface pr-10 pl-9 text-sm text-fg placeholder:text-fg-muted sm:h-9"
              />
              <kbd
                aria-hidden="true"
                className="pointer-events-none absolute top-1/2 right-2.5 hidden -translate-y-1/2 rounded border border-border px-1.5 py-0.5 font-mono text-2xs leading-none text-fg-muted sm:block"
              >
                /
              </kbd>
            </div>
            <div className="flex gap-2">
              <DropdownMenu>
                <DropdownMenuTrigger
                  className={cn(
                    'inline-flex h-11 items-center gap-1.5 rounded-md border px-3 text-sm sm:h-9',
                    tags.length
                      ? 'border-accent-line bg-accent-subtle text-accent'
                      : 'border-border-input bg-surface text-fg hover:bg-hover',
                  )}
                  aria-label={tags.length ? `Tags: ${tags.join(', ')}` : 'Filter by tag'}
                >
                  <Tag className="size-3.5" aria-hidden="true" />
                  Tags
                  {tags.length ? <span className="font-mono text-2xs">{tags.length}</span> : null}
                  <ChevronDown className="size-3.5 opacity-70" aria-hidden="true" />
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="max-h-80 overflow-y-auto">
                  <DropdownMenuLabel className="font-mono text-[10px] tracking-[0.12em] uppercase">
                    Runbooks with all of
                  </DropdownMenuLabel>
                  {tagOptions.length === 0 ? (
                    <p className="px-2 py-2 text-sm text-fg-muted">No tags on these runbooks.</p>
                  ) : (
                    tagOptions.map((tag) => (
                      <DropdownMenuCheckboxItem
                        key={tag}
                        checked={tags.includes(tag)}
                        onSelect={(event) => event.preventDefault()}
                        onCheckedChange={(checked) => toggleTag(tag, checked === true)}
                        className="font-mono text-xs"
                      >
                        {tag}
                      </DropdownMenuCheckboxItem>
                    ))
                  )}
                </DropdownMenuContent>
              </DropdownMenu>
              <Select value={effectiveSort} onValueChange={(value) => setSort(value as SortKey)}>
                <SelectTrigger aria-label="Sort runbooks" className="w-auto min-w-44 text-sm">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent align="end">
                  {sortOptions.map((option) => (
                    <SelectItem key={option.value} value={option.value}>
                      {option.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>

          {/* At a glance: last-run health, which also filters (needs run data) */}
          {catalog.data && !catalog.isError && runData ? (
            <div
              role="group"
              aria-label="Filter by last run"
              aria-describedby={loadedNote ? countsNoteId : undefined}
              className="-mx-1 flex gap-1 overflow-x-auto px-1 pb-0.5"
            >
              {HEALTH_FILTERS.map((option) => {
                const pressed = effectiveHealth === option.value;
                return (
                  <button
                    key={option.value}
                    type="button"
                    aria-pressed={pressed}
                    onClick={() => setHealth(option.value)}
                    className={cn(
                      'inline-flex h-8 shrink-0 items-center gap-1.5 rounded-full border px-3 text-xs transition-colors duration-[120ms]',
                      pressed
                        ? 'border-accent-line bg-accent-subtle font-medium text-accent'
                        : 'border-border text-fg-muted hover:bg-hover hover:text-fg',
                    )}
                  >
                    {option.label}
                    <span className="font-mono text-2xs tabular-nums">{counts[option.value]}</span>
                  </button>
                );
              })}
            </div>
          ) : null}

          {(filtered || effectiveHealth !== 'all') && (
            <div className="flex flex-wrap items-center gap-1.5 text-xs text-fg-muted">
              {tags.map((tag) => (
                <button
                  key={tag}
                  type="button"
                  onClick={() => toggleTag(tag, false)}
                  className="inline-flex items-center gap-1 rounded border border-border px-1.5 py-0.5 font-mono text-[11px] hover:bg-hover hover:text-fg"
                  aria-label={`Remove tag filter ${tag}`}
                >
                  {tag}
                  <X className="size-3" aria-hidden="true" />
                </button>
              ))}
              <button
                type="button"
                className="rounded px-1 text-accent hover:text-accent-strong hover:underline"
                onClick={clearFilters}
              >
                Clear filters
              </button>
            </div>
          )}

          {catalog.isError ? (
            <ErrorState
              message="Runbooks could not be loaded."
              code={catalog.error instanceof ApiError ? catalog.error.code : undefined}
              requestId={
                catalog.error instanceof ApiError
                  ? (catalog.error.requestId ?? undefined)
                  : undefined
              }
              onRetry={retry}
            />
          ) : !catalog.data || (catalog.isPlaceholderData && loaded.length === 0) ? (
            <ListSkeleton />
          ) : (
            <>
              {loadedNote ? (
                <p id={countsNoteId} role="note" className="text-xs text-fg-muted">
                  {loadedNote}
                </p>
              ) : null}

              {visible.length === 0 ? (
                <div className="rounded-lg border border-dashed border-border-strong px-6 py-10 text-center">
                  <p className="text-sm font-medium text-fg">No runbooks match</p>
                  <p className="mt-1 text-sm text-fg-muted">
                    {q ? `Nothing matches “${q}”` : 'Nothing matches these filters'}
                    {hasMore
                      ? filtered
                        ? ' among the runbooks searched so far.'
                        : ' among the runbooks loaded so far.'
                      : '.'}
                  </p>
                  <Button variant="secondary" size="sm" className="mt-4" onClick={clearFilters}>
                    Clear filters
                  </Button>
                </div>
              ) : (
                <RunbookList
                  rows={visible}
                  links={links}
                  canWrite={canWrite}
                  onNavigate={(href) => void navigate({ to: href })}
                  onStartRun={(row) => {
                    setStartRunbookId(row.runbook_id);
                    setStartOpen(true);
                  }}
                  renderLink={({ href, children, ...rest }) => (
                    <Link to={href} {...rest}>
                      {children}
                    </Link>
                  )}
                />
              )}

              <div className="flex flex-col items-center gap-2 pt-1 text-xs text-fg-muted sm:flex-row sm:justify-between">
                <p>
                  {visible.length === loaded.length
                    ? `${loaded.length} runbook${loaded.length === 1 ? '' : 's'}`
                    : `${visible.length} of ${loaded.length} loaded runbooks`}
                  {hasMore ? ' · more available' : ''}
                  <span className="hidden md:inline">
                    {' '}
                    · ↑↓ to move, Enter to open, / to search
                  </span>
                </p>
                {hasMore ? (
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={catalog.isFetchingNextPage}
                    onClick={() => void catalog.fetchNextPage()}
                  >
                    {catalog.isFetchingNextPage ? 'Loading…' : 'Load more'}
                  </Button>
                ) : null}
              </div>
            </>
          )}
        </div>
      )}

      {project && canWrite ? (
        <CreateRunbookDialog
          open={createOpen}
          onOpenChange={setCreateOpen}
          projectId={project.id}
          projectRef={projectRef}
        />
      ) : null}
      {project ? (
        <StartRunDialog
          open={startOpen}
          onOpenChange={setStartOpen}
          projectId={project.id}
          projectRef={projectRef}
          runbooks={loaded}
          initialRunbookId={startRunbookId}
        />
      ) : null}
    </>
  );
}
