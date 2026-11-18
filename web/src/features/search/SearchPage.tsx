import { Link, useNavigate } from '@tanstack/react-router';
import { useEffect, useState } from 'react';
import { BookOpenText, NotebookPen, Play, Sparkles, UserRound } from 'lucide-react';
import { ApiError } from '../../api/client';
import { EmptyState, ErrorState, PageHeader, Skeleton } from '../../ui/layout';
import { Input } from '../../ui/primitives/input';
import { StatusPill } from '../../ui/status';
import { dataSummary } from '../execution/runIdentity';
import { ago } from '../home/model';
import { useNow } from '../home/hooks';
import { useUniversalSearch } from './hooks';

export function SearchPage({
  initialQuery,
  onQueryChange,
}: {
  initialQuery: string;
  onQueryChange(q: string): void;
}) {
  const now = useNow();
  const navigate = useNavigate();
  const [text, setText] = useState(initialQuery);
  const [q, setQ] = useState(initialQuery);
  useEffect(() => {
    const timer = setTimeout(() => {
      setQ(text);
      onQueryChange(text);
    }, 250);
    return () => clearTimeout(timer);
  }, [text, onQueryChange]);
  const search = useUniversalSearch(q);
  const data = search.data;
  const empty =
    data &&
    data.runbooks.length === 0 &&
    data.runs.length === 0 &&
    data.steps.length === 0 &&
    data.memory.length === 0 &&
    data.skills.length === 0;

  return (
    <>
      <PageHeader
        title="Search"
        description="Across every project you can read. Choosing a result switches to its project."
      />
      <form
        role="search"
        onSubmit={(event) => {
          event.preventDefault();
          setQ(text);
          onQueryChange(text);
        }}
      >
        <label className="sr-only" htmlFor="universal-search">
          Search everything
        </label>
        <Input
          id="universal-search"
          type="search"
          placeholder="Search runbooks, runs, steps, memory, skills…"
          value={text}
          onChange={(event) => setText(event.target.value)}
        />
      </form>
      <div className="mt-6 grid gap-8">
        {q.trim() && search.isLoading ? (
          <div aria-busy="true" className="grid gap-2">
            <p role="status" className="sr-only">
              Searching…
            </p>
            <Skeleton className="h-16 w-full" />
            <Skeleton className="h-16 w-full" />
          </div>
        ) : null}
        {search.isError ? (
          <ErrorState
            message="Search could not be completed."
            code={search.error instanceof ApiError ? search.error.code : undefined}
            requestId={
              search.error instanceof ApiError ? (search.error.requestId ?? undefined) : undefined
            }
            onRetry={() => void search.refetch()}
          />
        ) : null}
        {data && empty ? (
          <EmptyState
            title="Nothing matched"
            description={`Nothing in your projects matches “${q}”.`}
          />
        ) : null}
        {data && data.runbooks.length > 0 ? (
          <section aria-label="Runbooks">
            <h2 className="mb-2 flex items-center gap-2 text-sm font-semibold">
              <BookOpenText className="size-4 text-fg-muted" aria-hidden="true" />
              Runbooks
            </h2>
            <ul className="divide-y divide-border rounded-lg border border-border bg-surface px-3">
              {data.runbooks.map((hit) => (
                <li key={hit.runbook_id} className="py-2">
                  <Link
                    className="text-accent hover:underline"
                    to="/p/$project/runbooks/$rb/editor"
                    params={{ project: hit.project_slug, rb: hit.runbook_id }}
                  >
                    {hit.title}
                  </Link>{' '}
                  <span className="text-xs text-fg-muted">
                    {hit.project_slug} · {hit.slug}
                  </span>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
        {data && data.runs.length > 0 ? (
          <section aria-label="Runs">
            <h2 className="mb-2 flex items-center gap-2 text-sm font-semibold">
              <Play className="size-4 text-fg-muted" aria-hidden="true" />
              Runs
            </h2>
            <ul className="divide-y divide-border rounded-lg border border-border bg-surface px-3">
              {data.runs.map((hit) => (
                <li key={hit.id} className="flex flex-wrap items-center gap-2">
                  <Link
                    className="text-accent hover:underline"
                    to="/x/$exe"
                    params={{ exe: hit.id }}
                  >
                    {hit.title || hit.id}
                  </Link>
                  <StatusPill status={hit.status} />
                  <span className="text-xs text-fg-muted">
                    {hit.project_slug}
                    {dataSummary(hit.fields) ? ` · ${dataSummary(hit.fields)}` : ''} · started{' '}
                    {ago(now - hit.created_at_ms)}
                  </span>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
        {data && data.steps.length > 0 ? (
          <section aria-label="Open steps">
            <h2 className="mb-2 flex items-center gap-2 text-sm font-semibold">
              <UserRound className="size-4 text-fg-muted" aria-hidden="true" />
              Open steps
            </h2>
            <ul className="divide-y divide-border rounded-lg border border-border bg-surface px-3">
              {data.steps.map((hit) => (
                <li key={hit.step_run_id} className="py-2">
                  <button
                    type="button"
                    className="text-accent hover:underline"
                    onClick={() =>
                      void navigate({ to: '/x/$exe', params: { exe: hit.execution_id } })
                    }
                  >
                    {hit.title}
                  </button>{' '}
                  <span className="text-xs text-fg-muted">
                    {hit.project_slug} · {hit.path}
                  </span>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
        {data && data.memory.length > 0 ? (
          <section aria-label="Memory">
            <h2 className="mb-2 flex items-center gap-2 text-sm font-semibold">
              <NotebookPen className="size-4 text-fg-muted" aria-hidden="true" />
              Memory
            </h2>
            <ul className="divide-y divide-border rounded-lg border border-border bg-surface px-3">
              {data.memory.map((hit) => (
                <li key={hit.memory_id} className="py-2">
                  <Link
                    className="text-accent hover:underline"
                    to="/p/$project/memory"
                    params={{ project: hit.project_slug }}
                  >
                    {hit.title}
                  </Link>{' '}
                  <span className="text-xs text-fg-muted">{hit.project_slug}</span>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
        {data && data.skills.length > 0 ? (
          <section aria-label="Skills">
            <h2 className="mb-2 flex items-center gap-2 text-sm font-semibold">
              <Sparkles className="size-4 text-fg-muted" aria-hidden="true" />
              Skills
            </h2>
            <ul className="divide-y divide-border rounded-lg border border-border bg-surface px-3">
              {data.skills.map((hit) => (
                <li key={`${hit.project_id}-${hit.name}`} className="py-2">
                  <Link
                    className="text-accent hover:underline"
                    to="/p/$project/skills"
                    params={{ project: hit.project_slug }}
                  >
                    {hit.name}
                  </Link>{' '}
                  <span className="text-xs text-fg-muted">{hit.project_slug}</span>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
      </div>
    </>
  );
}
