import { Sparkline } from '../../ui/status/Sparkline';
// UI-2: the project's Home page. It answers, in five seconds, what needs
// me, what's running, what broke and what happened. Built for a team whose
// runbooks are mostly agentic ops checks plus onboarding flows that mix
// people and customers, so "waiting on" names who or what: an agent, a
// person, an approval, a signal from a customer or an API, or a timer.
//
// Each section reads on its own (hooks.ts) and fails on its own: one
// section's error never blanks the page. Left (wider): Needs you, Running
// now, Failed. Right: Health, Activity. One column on phones. A project
// with no runbooks gets the getting-started checklist instead.
import { Link } from '@tanstack/react-router';
import {
  ArrowRight,
  Bot,
  Check,
  CheckCircle2,
  CircleCheck,
  CircleDashed,
  Clock3,
  FilePlus2,
  FolderX,
  Hand,
  Play,
  ShieldCheck,
  Terminal,
  UserRound,
  XCircle,
  type LucideIcon,
} from 'lucide-react';
import { useEffect, useId, useMemo, useState, type ReactNode } from 'react';
import { toast } from 'sonner';
import { ApiError } from '../../api/client';
import { useMe, useRunbooks } from '../../api/hooks';
import { EmptyState, Skeleton } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import { cn } from '../../ui/primitives/cn';
import { StatusPill, WaitBadge } from '../../ui/status';
import { dataSummary, useRunIdentities } from '../execution/runIdentity';
import { useOrgContext } from '../context/OrgContext';
import { useDirectoryNames } from '../context/names';
import { CreateRunbookDialog } from '../editor/CreateRunbookDialog';
import { editorPermissions } from '../editor/hooks';
import { useInbox } from '../inbox/hooks';
import {
  displayTitle,
  healthCounts,
  hasRunData,
  runHealth,
  type RunbookRow,
} from '../runbooks/catalog';
import { StartRunDialog } from '../start/StartRunDialog';
import {
  HOME_LIST_LIMIT,
  HOME_WINDOW_DAYS,
  useNow,
  useRecentRuns,
  useRunDocuments,
  useRunsWithStatus,
  type RecentRuns,
} from './hooks';
import {
  ALL_PASSED_NOTE,
  OPEN_STATUSES,
  dailyRuns,
  elapsed,
  failingStep,
  firstName,
  greeting,
  needsYou,
  principalActor,
  progressOf,
  quietFailedNote,
  runActivity,
  shortAge,
  ago,
  todayRuns,
  waitingOn,
  windowStart,
  type RunDocument,
  type RunEntry,
} from './model';

/** The one command that gives an agent The Runbook's skill. */
export const AGENT_SKILL_COMMAND = 'runbook skill install --agent claude --therunbook';

export function HomePage({ projectRef, now: nowProp }: { projectRef: string; now?: number }) {
  const { resolveProject, isLoading: orgLoading } = useOrgContext();
  const project = resolveProject(projectRef);
  const me = useMe();
  const { canWrite } = editorPermissions(me.data?.credential.scopes);
  // Ticks (every 30 s and on returning to the tab), so ages advance and
  // "today", the 7-day window and the greeting's date turn over at midnight.
  const now = useNow(nowProp);
  const runbooks = useRunbooks(project?.id);
  const [startOpen, setStartOpen] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);

  if (!project) {
    return orgLoading ? <HomeSkeleton /> : <NoSuchProject projectRef={projectRef} />;
  }
  const rows = (runbooks.data ?? []) as RunbookRow[];
  const empty = runbooks.isSuccess && rows.length === 0;

  return (
    <div data-slot="home" className="grid w-full gap-6">
      <GreetingStrip
        name={firstName(me.data?.user?.name)}
        projectTitle={project.title}
        now={now}
        canWrite={canWrite}
        canStart={rows.length > 0}
        onStart={() => setStartOpen(true)}
        onCreate={() => setCreateOpen(true)}
      />
      {empty ? (
        <GettingStarted
          projectRef={project.slug}
          canWrite={canWrite}
          onCreate={() => setCreateOpen(true)}
        />
      ) : (
        <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(300px,360px)]">
          <div className="grid min-w-0 gap-6">
            <NeedsYou projectId={project.id} now={now} />
            <RunningNow
              projectId={project.id}
              projectRef={project.slug}
              now={now}
              runbooks={rows}
            />
            <Failed
              projectId={project.id}
              projectRef={project.slug}
              now={now}
              runbooks={runbooks}
            />
          </div>
          <div className="grid min-w-0 gap-6">
            <Health
              projectId={project.id}
              projectRef={project.slug}
              now={now}
              runbooks={runbooks}
            />
            <Activity projectId={project.id} projectRef={project.slug} now={now} />
          </div>
        </div>
      )}
      {canWrite ? (
        <CreateRunbookDialog
          open={createOpen}
          onOpenChange={setCreateOpen}
          projectId={project.id}
          projectRef={project.slug}
        />
      ) : null}
      <StartRunDialog
        open={startOpen}
        onOpenChange={setStartOpen}
        projectId={project.id}
        projectRef={project.slug}
        runbooks={rows}
      />
    </div>
  );
}

function HomeSkeleton() {
  return (
    <div aria-busy="true" className="grid gap-6">
      <p role="status" className="sr-only">
        Loading Home…
      </p>
      <Skeleton className="h-28 w-full" />
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_340px]">
        <Skeleton className="h-64 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    </div>
  );
}

/** A project this person cannot see (or that does not exist): say so, and offer theirs. */
function NoSuchProject({ projectRef }: { projectRef: string }) {
  const { projects } = useOrgContext();
  return (
    <div data-slot="home-no-project" className="mx-auto grid w-full max-w-lg gap-4 py-8">
      <EmptyState
        icon={FolderX}
        headingLevel={1}
        title="Project not found, or no access"
        description={`No project “${projectRef}” is visible to you. It may not exist, or you may not be a member.`}
        action={
          projects.length > 0 ? (
            <nav aria-label="Your projects">
              <ul className="grid gap-1 text-sm">
                {projects.map((p) => (
                  <li key={p.id}>
                    <Link
                      to="/p/$project"
                      params={{ project: p.slug }}
                      className="font-medium text-accent hover:text-accent-strong hover:underline"
                    >
                      {p.title}
                    </Link>
                  </li>
                ))}
              </ul>
            </nav>
          ) : (
            <p className="text-sm text-fg-muted">You have no projects in this organization yet.</p>
          )
        }
      />
    </div>
  );
}

// ---- Greeting strip

/** Copies `text`; says "Copied" on the button and to screen readers for 2 s. */
function CopyCommandButton({
  text,
  what,
  children,
  size,
}: {
  text: string;
  what: string;
  children: ReactNode;
  size?: 'sm';
}) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 2000);
    return () => clearTimeout(timer);
  }, [copied]);
  function copy() {
    const write = navigator.clipboard?.writeText(text);
    if (!write) {
      toast.error('Could not copy; select the command instead.');
      return;
    }
    void write.then(
      () => setCopied(true),
      () => toast.error('Could not copy; select the command instead.'),
    );
  }
  return (
    <>
      <Button variant="secondary" size={size} onClick={copy} title={text}>
        {copied ? (
          <>
            <Check aria-hidden="true" />
            Copied
          </>
        ) : (
          children
        )}
      </Button>
      <span role="status" className="sr-only">
        {copied ? `${what} copied: ${text}` : ''}
      </span>
    </>
  );
}

function GreetingStrip({
  name,
  projectTitle,
  now,
  canWrite,
  canStart,
  onStart,
  onCreate,
}: {
  name: string;
  projectTitle: string;
  now: number;
  canWrite: boolean;
  canStart: boolean;
  onStart(): void;
  onCreate(): void;
}) {
  const date = new Date(now).toLocaleDateString([], {
    weekday: 'long',
    month: 'long',
    day: 'numeric',
  });
  return (
    <section
      aria-labelledby="home-greeting"
      data-slot="home-greeting"
      className="relative overflow-hidden rounded-lg border border-border bg-surface px-5 py-5 sm:px-6"
    >
      {/* The page's one Angler gradient: a hairline along the top. */}
      <span
        aria-hidden="true"
        className="absolute inset-x-0 top-0 h-[3px] bg-gradient-to-r from-accent via-accent to-mint"
      />
      <div className="flex flex-col gap-4 md:flex-row md:items-end md:justify-between">
        <div className="min-w-0">
          <p className="mb-1.5 font-mono text-[11px] font-medium tracking-[0.1em] text-fg-muted uppercase">
            {date} · {projectTitle}
          </p>
          <h1 id="home-greeting" className="text-xl font-semibold tracking-[-0.02em] text-fg">
            {greeting(new Date(now))}
            {name ? `, ${name}` : ''}
          </h1>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button onClick={onStart} disabled={!canStart}>
            <Play aria-hidden="true" />
            Start a run
          </Button>
          {canWrite ? (
            <Button variant="secondary" onClick={onCreate}>
              <FilePlus2 aria-hidden="true" />
              New runbook
            </Button>
          ) : null}
          <CopyCommandButton text={AGENT_SKILL_COMMAND} what="Agent setup command">
            <Bot aria-hidden="true" />
            Create with your agent
          </CopyCommandButton>
        </div>
      </div>
    </section>
  );
}

// ---- Section frame

function Section({
  id,
  title,
  icon: Icon,
  count,
  viewAll,
  note,
  children,
  className,
}: {
  id: string;
  title: string;
  icon: LucideIcon;
  count?: ReactNode;
  viewAll?: { to: string; label: string; search?: Record<string, string> };
  /** One muted line under the heading: what the section covers. */
  note?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  const headingId = useId();
  return (
    <section
      aria-labelledby={headingId}
      data-slot={`home-${id}`}
      className={cn('grid min-w-0 gap-2.5', className)}
    >
      <div className="flex items-center justify-between gap-3">
        <h2 id={headingId} className="flex items-center gap-2 text-sm font-semibold text-fg">
          <Icon className="size-4 text-fg-muted" aria-hidden="true" />
          {title}
          {count !== undefined && count !== null ? (
            <span className="rounded border border-border px-1.5 py-0.5 font-mono text-2xs leading-none font-normal text-fg-muted">
              {count}
            </span>
          ) : null}
        </h2>
        {viewAll ? (
          <Link
            to={viewAll.to}
            search={viewAll.search}
            className="inline-flex items-center gap-1 rounded text-xs font-medium text-accent hover:text-accent-strong hover:underline focus-visible:outline-2 focus-visible:outline-focus"
            aria-label={viewAll.label}
          >
            View all
            <ArrowRight className="size-3" aria-hidden="true" />
          </Link>
        ) : null}
      </div>
      {note ? <p className="-mt-1 text-xs text-fg-muted">{note}</p> : null}
      {children}
    </section>
  );
}

function Card({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={cn('rounded-lg border border-border bg-surface', className)}>{children}</div>
  );
}

function SectionSkeleton({ rows = 3, label }: { rows?: number; label: string }) {
  return (
    <div aria-busy="true" className="grid gap-3 rounded-lg border border-border bg-surface p-4">
      <p role="status" className="sr-only">
        Loading {label}…
      </p>
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className="h-10 w-full" />
      ))}
    </div>
  );
}

function SectionError({ what, error, onRetry }: { what: string; error: unknown; onRetry(): void }) {
  const code = error instanceof ApiError ? error.code : undefined;
  return (
    <Card className="flex flex-wrap items-center justify-between gap-3 p-4">
      <p role="alert" className="text-sm text-fg">
        {what} could not be loaded.
        {code ? <span className="ml-1 font-mono text-2xs text-fg-muted">{code}</span> : null}
      </p>
      <Button variant="secondary" size="sm" onClick={onRetry}>
        Retry
      </Button>
    </Card>
  );
}

function SectionEmpty({
  icon: Icon,
  children,
  tone = 'ok',
}: {
  icon: LucideIcon;
  children: ReactNode;
  /** `neutral` when the empty state is not plainly good news. */
  tone?: 'ok' | 'neutral';
}) {
  return (
    <Card className="flex items-center gap-3 px-4 py-5 text-sm text-fg-muted">
      <Icon
        className={cn('size-4 shrink-0', tone === 'ok' ? 'text-mint-text' : 'text-fg-muted')}
        aria-hidden="true"
      />
      <p>{children}</p>
    </Card>
  );
}

const rowLink =
  'flex min-w-0 items-start gap-3 px-4 py-3 hover:bg-hover focus-visible:bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus';

// ---- Needs you

function NeedsYou({ projectId, now }: { projectId: string; now: number }) {
  const inbox = useInbox();
  const { items, total } = needsYou(inbox.data?.items ?? [], projectId, HOME_LIST_LIMIT);
  const identities = useRunIdentities(
    items.map((task) => task.execution_id),
    useDirectoryNames(),
  );
  return (
    <Section
      id="needs-you"
      title="Needs you"
      icon={Hand}
      count={inbox.data ? (inbox.data.truncated ? `${total}+` : total) : undefined}
      viewAll={{ to: '/tasks', label: 'View all your tasks' }}
    >
      {inbox.isLoading ? (
        <SectionSkeleton rows={2} label="your tasks" />
      ) : inbox.isError ? (
        <SectionError what="Your tasks" error={inbox.error} onRetry={() => void inbox.refetch()} />
      ) : items.length === 0 ? (
        <SectionEmpty icon={CircleCheck}>Nothing needs you.</SectionEmpty>
      ) : (
        <Card>
          <ul className="divide-y divide-border">
            {items.map((task) => {
              const approval = task.kind === 'approval';
              const Icon = approval ? ShieldCheck : UserRound;
              return (
                <li key={task.step_run_id}>
                  <Link
                    to="/x/$exe"
                    params={{ exe: task.execution_id }}
                    className={cn(rowLink, 'items-center')}
                  >
                    <span
                      className={cn(
                        'flex size-8 shrink-0 items-center justify-center rounded-md',
                        approval
                          ? 'bg-status-paused-bg text-status-paused-fg'
                          : 'bg-accent-subtle text-accent',
                      )}
                    >
                      <Icon className="size-4" aria-hidden="true" />
                    </span>
                    <span className="grid min-w-0 flex-1 gap-0.5">
                      <span className="truncate text-sm font-medium text-fg">{task.title}</span>
                      <span className="truncate text-xs text-fg-muted">
                        {approval ? 'Your approval' : 'Your task'} in{' '}
                        {task.execution_title || task.execution_id}
                        {identities.get(task.execution_id)?.summary
                          ? ` · ${identities.get(task.execution_id)?.summary}`
                          : ''}
                      </span>
                    </span>
                    <span className="flex shrink-0 flex-col items-end gap-0.5">
                      <span className="text-xs font-medium text-accent">
                        {approval ? 'Decide' : 'Open'}
                        <ArrowRight className="ml-0.5 inline size-3" aria-hidden="true" />
                      </span>
                      {task.since_ms ? (
                        <span className="font-mono text-2xs text-fg-muted">
                          waiting {shortAge(now - task.since_ms)}
                        </span>
                      ) : null}
                    </span>
                  </Link>
                </li>
              );
            })}
          </ul>
        </Card>
      )}
    </Section>
  );
}

// ---- Running now

// UI-3: the badge moved to ui/status so the run graph shares it.
export { WaitBadge };

function ProgressBar({ done, total }: { done: number; total: number }) {
  const percent = Math.round((done / total) * 100);
  return (
    <span className="flex items-center gap-2">
      <span
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={total}
        aria-valuenow={done}
        aria-label={`${done} of ${total} steps done`}
        className="relative h-1.5 w-24 overflow-hidden rounded-full bg-hover"
      >
        <span
          className="absolute inset-y-0 left-0 rounded-full bg-accent"
          style={{ width: `${percent}%` }}
        />
      </span>
      <span className="font-mono text-2xs text-fg-muted">
        {done}/{total}
      </span>
    </span>
  );
}

function runbookSlugs(rows: readonly RunbookRow[]) {
  return new Map(rows.map((row) => [row.runbook_id, row]));
}

function RunningNow({
  projectId,
  projectRef,
  now,
  runbooks,
}: {
  projectId: string;
  projectRef: string;
  now: number;
  runbooks: RunbookRow[];
}) {
  const runs = useRunsWithStatus(projectId, OPEN_STATUSES);
  const items = runs.data?.items ?? [];
  const documents = useRunDocuments(items.map((run) => run.id));
  const names = useDirectoryNames();
  const byId = runbookSlugs(runbooks);
  const more = Boolean(runs.data?.next_cursor);
  return (
    <Section
      id="running"
      title="Running now"
      icon={Play}
      count={runs.data ? `${items.length}${more ? '+' : ''}` : undefined}
      viewAll={{
        to: `/p/${projectRef}/executions`,
        search: { status: OPEN_STATUSES.join(',') },
        label: 'View all open runs',
      }}
    >
      {runs.isLoading ? (
        <SectionSkeleton label="open runs" />
      ) : runs.isError ? (
        <SectionError what="Open runs" error={runs.error} onRetry={() => void runs.refetch()} />
      ) : items.length === 0 ? (
        <SectionEmpty icon={CircleDashed}>Nothing is running.</SectionEmpty>
      ) : (
        <Card>
          <ul className="divide-y divide-border">
            {items.map((run, index) => {
              const document = documents[index]?.data;
              const progress = document ? progressOf(document.header) : null;
              const waits = document ? waitingOn(document, names, 3, now) : [];
              const runbook = byId.get(run.runbook_id);
              const since = document?.header.started_at_ms ?? run.created_at_ms;
              return (
                <li key={run.id}>
                  <Link to="/x/$exe" params={{ exe: run.id }} className={rowLink}>
                    <span className="grid min-w-0 flex-1 gap-1.5">
                      <span className="flex min-w-0 items-center gap-2">
                        <span className="truncate text-sm font-medium text-fg">{run.title}</span>
                        {runbook ? (
                          <span className="hidden shrink-0 font-mono text-2xs text-fg-muted sm:inline">
                            {runbook.slug}
                          </span>
                        ) : null}
                        {document && dataSummary(document.header.inputs) ? (
                          <span className="truncate text-2xs text-fg-muted">
                            {dataSummary(document.header.inputs)}
                          </span>
                        ) : null}
                      </span>
                      <span className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
                        {progress ? <ProgressBar {...progress} /> : null}
                        {document ? (
                          waits.map((wait) => (
                            <WaitBadge key={`${wait.kind}:${wait.label}`} {...wait} />
                          ))
                        ) : documents[index]?.isLoading ? (
                          <Skeleton className="h-4 w-28" />
                        ) : null}
                      </span>
                    </span>
                    <span className="flex shrink-0 flex-col items-end gap-1">
                      <StatusPill status={run.status} />
                      <span className="font-mono text-2xs text-fg-muted" title="Elapsed">
                        {elapsed(now - since)}
                      </span>
                    </span>
                  </Link>
                </li>
              );
            })}
          </ul>
        </Card>
      )}
    </Section>
  );
}

// ---- Failed or needs attention

function Failed({
  projectId,
  projectRef,
  now,
  runbooks,
}: {
  projectId: string;
  projectRef: string;
  now: number;
  runbooks: ReturnType<typeof useRunbooks>;
}) {
  // Bounded like Health and Activity: failures of runs started in the
  // window, so the section empties once a week passes without one.
  const fromMs = useMemo(() => windowStart(now, HOME_WINDOW_DAYS), [now]);
  const runs = useRunsWithStatus(projectId, ['failed'], fromMs);
  const items = runs.data?.items ?? [];
  const documents = useRunDocuments(items.map((run) => run.id));
  const rows = (runbooks.data ?? []) as RunbookRow[];
  const quiet = quietFailedNote({
    rows: runbooks.data as RunbookRow[] | undefined,
    isError: runbooks.isError,
  });
  const shown = new Set(items.map((run) => run.runbook_id));
  // Runbooks whose newest run failed, beyond the runs listed above.
  const failing = rows
    .filter((row) => runHealth(row) === 'failing' && !shown.has(row.runbook_id))
    .slice(0, 3);
  return (
    <Section
      id="failed"
      title="Recently failed"
      icon={XCircle}
      note={`Runs started in the last ${HOME_WINDOW_DAYS} days that failed, and runbooks whose last run failed.`}
      viewAll={{
        to: `/p/${projectRef}/executions`,
        search: { status: 'failed' },
        label: 'View all failed runs',
      }}
    >
      {runs.isLoading ? (
        <SectionSkeleton rows={2} label="failed runs" />
      ) : runs.isError ? (
        <SectionError what="Failed runs" error={runs.error} onRetry={() => void runs.refetch()} />
      ) : items.length === 0 && failing.length === 0 ? (
        <SectionEmpty icon={CheckCircle2} tone={quiet === ALL_PASSED_NOTE ? 'ok' : 'neutral'}>
          No failed runs in the last {HOME_WINDOW_DAYS} days.{quiet ? ` ${quiet}` : ''}
        </SectionEmpty>
      ) : (
        <Card>
          <ul className="divide-y divide-border">
            {items.map((run, index) => (
              <FailedRunRow
                key={run.id}
                run={run}
                document={documents[index]?.data}
                loading={documents[index]?.isLoading ?? false}
                now={now}
              />
            ))}
            {failing.map((row) => (
              <li key={row.runbook_id}>
                <Link
                  to="/p/$project/runbooks/$rb"
                  params={{ project: projectRef, rb: row.runbook_id }}
                  className={cn(rowLink, 'items-center')}
                >
                  <XCircle className="size-4 shrink-0 text-danger-text" aria-hidden="true" />
                  <span className="grid min-w-0 flex-1 gap-0.5">
                    <span className="truncate text-sm font-medium text-fg">
                      {displayTitle(row)}
                    </span>
                    <span className="text-xs text-fg-muted">Its last run failed</span>
                  </span>
                  {row.last_run ? (
                    <span className="shrink-0 font-mono text-2xs text-fg-muted">
                      {ago(now - (row.last_run.completed_at_ms ?? row.last_run.created_at_ms))}
                    </span>
                  ) : null}
                </Link>
              </li>
            ))}
          </ul>
        </Card>
      )}
    </Section>
  );
}

function FailedRunRow({
  run,
  document,
  loading,
  now,
}: {
  run: RunEntry;
  document: RunDocument | undefined;
  loading: boolean;
  now: number;
}) {
  const step = document ? failingStep(document) : null;
  return (
    <li>
      <Link to="/x/$exe" params={{ exe: run.id }} className={rowLink}>
        <XCircle className="mt-0.5 size-4 shrink-0 text-danger-text" aria-hidden="true" />
        <span className="grid min-w-0 flex-1 gap-0.5">
          <span className="truncate text-sm font-medium text-fg">{run.title}</span>
          {step ? (
            <span className="min-w-0 text-xs text-fg-muted">
              <span className="text-fg">Failed at {step.title}</span>
              {step.summary ? <span className="line-clamp-2"> — {step.summary}</span> : null}
            </span>
          ) : loading ? (
            <Skeleton className="h-3.5 w-48" />
          ) : (
            <span className="text-xs text-fg-muted">The run failed.</span>
          )}
        </span>
        <span className="shrink-0 font-mono text-2xs text-fg-muted">
          {ago(now - (run.completed_at_ms ?? run.created_at_ms))}
        </span>
      </Link>
    </li>
  );
}

// ---- Health at a glance

function Stat({ label, value, dot }: { label: string; value: ReactNode; dot: string }) {
  return (
    <div className="grid gap-1.5 rounded-md px-3 py-2.5">
      <span className="font-mono text-lg leading-none font-medium text-fg">{value}</span>
      <span className="flex items-center gap-1.5 text-2xs text-fg-muted">
        <span className={cn('size-1.5 rounded-full', dot)} aria-hidden="true" />
        {label}
      </span>
    </div>
  );
}

function Health({
  projectId,
  projectRef,
  now,
  runbooks,
}: {
  projectId: string;
  projectRef: string;
  now: number;
  runbooks: ReturnType<typeof useRunbooks>;
}) {
  const fromMs = useMemo(() => windowStart(now, HOME_WINDOW_DAYS), [now]);
  const recent = useRecentRuns(projectId, fromMs);
  const rows = (runbooks.data ?? []) as RunbookRow[];
  const counts = healthCounts(rows);
  const withRuns = hasRunData(rows);
  return (
    <Section
      id="health"
      title="Health at a glance"
      icon={CircleCheck}
      viewAll={{ to: `/p/${projectRef}/runbooks`, label: 'View all runbooks' }}
    >
      <Card className="grid gap-1 p-2">
        {runbooks.isLoading ? (
          <Skeleton className="m-2 h-14" />
        ) : runbooks.isError ? (
          <SectionError
            what="Runbooks"
            error={runbooks.error}
            onRetry={() => void runbooks.refetch()}
          />
        ) : withRuns || rows.length === 0 ? (
          <div role="group" className="grid grid-cols-3" aria-label="Runbooks by their last run">
            <Stat label="Passing" value={counts.passing} dot="bg-mint" />
            <Stat label="Failing" value={counts.failing} dot="bg-danger" />
            <Stat label="Never run" value={counts.never} dot="bg-border-strong" />
          </div>
        ) : (
          <p className="px-3 py-2 text-xs text-fg-muted">
            Your access shows runbooks but not their runs.
          </p>
        )}
        <div className="mx-2 border-t border-border" />
        <TodayRuns recent={recent} now={now} />
      </Card>
    </Section>
  );
}

function TodayRuns({ recent, now }: { recent: ReturnType<typeof useRecentRuns>; now: number }) {
  if (recent.isLoading) return <Skeleton className="m-2 h-14" />;
  if (recent.isError)
    return (
      <SectionError
        what="Today's runs"
        error={recent.error}
        onRetry={() => void recent.refetch()}
      />
    );
  const data = recent.data as RecentRuns;
  const today = todayRuns(data, now);
  const more = today.capped ? '+' : '';
  const series = dailyRuns(data.entries, now, HOME_WINDOW_DAYS, data.complete);
  const showSpark = series && series.filter((n) => n > 0).length >= 2;
  return (
    <div className="grid gap-2 px-3 py-2.5">
      <div className="flex items-end justify-between gap-3">
        <div className="grid gap-1">
          <span className="text-2xs font-medium tracking-wide text-fg-muted uppercase">
            Runs today
          </span>
          <span
            className="font-mono text-lg leading-none font-medium text-fg"
            title={
              today.capped
                ? 'More runs started today than one read covers; these are the newest.'
                : undefined
            }
          >
            {today.started}
            {more}
          </span>
        </div>
        {showSpark && series ? (
          <div className="grid justify-items-end gap-1">
            <Sparkline
              series={series}
              label={`Runs started per day, last ${HOME_WINDOW_DAYS} days: ${series.join(', ')}`}
            />
            <span className="text-2xs text-fg-muted">last {HOME_WINDOW_DAYS} days</span>
          </div>
        ) : null}
      </div>
      <p className="flex flex-wrap gap-x-3 gap-y-1 text-xs text-fg-muted">
        <span>
          <span className="font-medium text-mint-text">
            {today.succeeded}
            {more}
          </span>{' '}
          succeeded
        </span>
        <span>
          <span className="font-medium text-danger-text">
            {today.failed}
            {more}
          </span>{' '}
          failed
        </span>
        <span>
          <span className="font-medium text-fg">
            {today.open}
            {more}
          </span>{' '}
          open
        </span>
      </p>
    </div>
  );
}

// ---- Recent activity

function Activity({
  projectId,
  projectRef,
  now,
}: {
  projectId: string;
  projectRef: string;
  now: number;
}) {
  const fromMs = useMemo(() => windowStart(now, HOME_WINDOW_DAYS), [now]);
  const recent = useRecentRuns(projectId, fromMs);
  const names = useDirectoryNames();
  const items = recent.data ? runActivity(recent.data.entries, 8) : [];
  return (
    <Section
      id="activity"
      title="Recent activity"
      icon={Clock3}
      viewAll={{ to: `/p/${projectRef}/executions`, label: 'View all runs' }}
    >
      {recent.isLoading ? (
        <SectionSkeleton rows={4} label="recent activity" />
      ) : recent.isError ? (
        <SectionError
          what="Recent activity"
          error={recent.error}
          onRetry={() => void recent.refetch()}
        />
      ) : items.length === 0 ? (
        <SectionEmpty icon={CircleDashed}>
          No runs in the last {HOME_WINDOW_DAYS} days.
        </SectionEmpty>
      ) : (
        <Card>
          <ol className="divide-y divide-border">
            {items.map((item) => {
              const actor = principalActor(item.run.created_by, names.nameOf);
              const finished = item.kind === 'finished';
              const ok = item.run.status === 'succeeded';
              const Icon = !finished ? Play : ok ? CheckCircle2 : XCircle;
              return (
                <li key={item.key}>
                  <Link
                    to="/x/$exe"
                    params={{ exe: item.run.id }}
                    className={cn(rowLink, 'py-2.5')}
                  >
                    <Icon
                      className={cn(
                        'mt-0.5 size-3.5 shrink-0',
                        !finished ? 'text-accent' : ok ? 'text-mint-text' : 'text-fg-muted',
                        finished && item.run.status === 'failed' && 'text-danger-text',
                      )}
                      aria-hidden="true"
                    />
                    <span className="min-w-0 flex-1 text-xs break-words text-fg-muted">
                      {finished ? (
                        <>
                          <span className="font-medium text-fg">{item.run.title}</span>{' '}
                          {item.run.status === 'succeeded'
                            ? 'succeeded'
                            : item.run.status === 'failed'
                              ? 'failed'
                              : item.run.status}
                        </>
                      ) : (
                        <>
                          <ActorName kind={actor.kind} label={actor.label} /> started{' '}
                          <span className="font-medium text-fg">{item.run.title}</span>
                        </>
                      )}
                    </span>
                    <span className="shrink-0 font-mono text-2xs text-fg-muted">
                      {shortAge(now - item.atMs)}
                    </span>
                  </Link>
                </li>
              );
            })}
          </ol>
        </Card>
      )}
    </Section>
  );
}

function ActorName({ kind, label }: { kind: string; label: string }) {
  const Icon =
    kind === 'person' ? UserRound : kind === 'agent' || kind === 'service' ? Bot : Terminal;
  return (
    // Inline, not flex: in a sentence a flex box sits on its icon's edge and
    // lifts the name off the line the rest of the text shares.
    <span className="font-medium text-fg">
      <Icon
        className="mr-1 inline-block size-3 align-[-0.125em] text-fg-muted"
        aria-hidden="true"
      />
      {label}
    </span>
  );
}

// ---- Getting started (a project with no runbooks)

function GettingStarted({
  projectRef,
  canWrite,
  onCreate,
}: {
  projectRef: string;
  canWrite: boolean;
  onCreate(): void;
}) {
  const steps: { title: string; body: ReactNode; action?: ReactNode }[] = [
    {
      title: 'Create or import a runbook',
      body: 'Start from a short form or paste YAML; or push a file with the CLI.',
      action: canWrite ? (
        <Button size="sm" onClick={onCreate}>
          New runbook
        </Button>
      ) : (
        <Link
          to="/p/$project/runbooks"
          params={{ project: projectRef }}
          className="text-xs font-medium text-accent hover:underline"
        >
          Open Runbooks
        </Link>
      ),
    },
    {
      title: 'Connect an agent',
      body: (
        <>
          Give your agent the skill with <Code>{AGENT_SKILL_COMMAND}</Code>, or connect its MCP
          client with <Code>runbook mcp serve</Code>.
        </>
      ),
      action: (
        <CopyCommandButton size="sm" text={AGENT_SKILL_COMMAND} what="Agent setup command">
          Copy command
        </CopyCommandButton>
      ),
    },
    {
      title: 'Start a first run',
      body: 'Publish the runbook, then start it from its page or with runbook run.',
      action: (
        <Link
          to="/p/$project/runbooks"
          params={{ project: projectRef }}
          className="text-xs font-medium text-accent hover:underline"
        >
          Open Runbooks
        </Link>
      ),
    },
    {
      title: 'Invite a teammate',
      body: 'Add people to this project and choose their role.',
      action: (
        <Link
          to="/p/$project/settings"
          params={{ project: projectRef }}
          className="text-xs font-medium text-accent hover:underline"
        >
          Project members
        </Link>
      ),
    },
  ];
  return (
    <section
      aria-labelledby="home-getting-started"
      data-slot="home-getting-started"
      className="grid gap-3"
    >
      <div>
        <h2 id="home-getting-started" className="text-sm font-semibold text-fg">
          Get this project going
        </h2>
        <p className="mt-1 text-sm text-fg-muted">
          Four steps from an empty project to runs your agents and team share.
        </p>
      </div>
      <Card>
        <ol className="divide-y divide-border">
          {steps.map((step, index) => (
            <li
              key={step.title}
              className="flex flex-col gap-3 px-4 py-4 sm:flex-row sm:items-center"
            >
              <span
                aria-hidden="true"
                className="flex size-7 shrink-0 items-center justify-center rounded-full border border-border-strong font-mono text-xs text-fg-muted"
              >
                {index + 1}
              </span>
              <span className="grid min-w-0 flex-1 gap-0.5">
                <span className="text-sm font-medium text-fg">{step.title}</span>
                <span className="text-xs leading-relaxed text-fg-muted">{step.body}</span>
              </span>
              {step.action ? <span className="shrink-0">{step.action}</span> : null}
            </li>
          ))}
        </ol>
      </Card>
    </section>
  );
}

function Code({ children }: { children: ReactNode }) {
  return (
    <code className="rounded border border-border bg-surface-raised px-1 py-0.5 font-mono text-[11px] text-fg">
      {children}
    </code>
  );
}
