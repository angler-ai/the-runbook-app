// The run page at /x/$exe. UI-3: a header (what ran, how it is going,
// who started it, its inputs, its actions) over three views in `?view=`:
// Graph (default, the runbook's graph with the run's state over it),
// Document (the run as a page) and Steps (M2's step rail and sections with
// lazy logs and inline actions). One live stream refreshes all three.
import { useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { Download, Radio } from 'lucide-react';
import { ApiError, api } from '../../api/client';
import { useExecutionDocument, useMe } from '../../api/hooks';
import { ConfirmDialog } from '../../ui/actions';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '../../ui/primitives/dropdown-menu';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../../ui/primitives/tabs';
import {
  isContainerSection,
  sectionDisplayStatus,
  StepRail,
  StepSection,
  stepHeadingId,
  UndeliveredNotice,
} from '../../ui/execution';
import { EmptyState, ErrorState, Skeleton, SplitPane } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import {
  capabilities,
  CONTROL_SCOPE_HINT,
  controlError as controlErrorText,
  friendlyError,
  isEligibleToDecide,
} from './gating';
import {
  downloadReport,
  useExecutionControl,
  useMyProjectRole,
  useRerun,
  useClaimStep,
  useSignal,
  useStepLogs,
} from './hooks';
import { statusWords, useStatusAnnouncements } from './announce';
import { useLiveExecution } from './live';
import { StepActions } from './StepActions';
import { tabClaims } from './session';
import { RunnerStep } from './RunnerStep';
import { StepAssignment } from './StepAssignment';
import { useDirectoryNames, type DirectoryNames } from '../context/names';
import { useOrgContext } from '../context/OrgContext';
import type { CompilerBackend } from '../compile/useCompiler';
import { RunHeader } from './RunHeader';
import { RunGraphView, useRunOutline } from './RunGraphView';
import { RunDocumentView } from './RunDocumentView';
import { RunChecklistView } from './RunChecklistView';
import { RUN_VIEW_LABELS, RUN_VIEWS, type RunView } from './views';
import type { RunDocument } from './overlay';

const TERMINAL = new Set(['succeeded', 'failed', 'cancelled']);
const APP_TITLE = 'The Runbook';

/** Focus a step's heading (its section's focus target), without a second scroll. */
export function focusStep(stepRunId: string) {
  document.getElementById(stepHeadingId(stepRunId))?.focus({ preventScroll: true });
}
const ACTIVE_STEP = new Set(['ready', 'claimed', 'waiting']);
/** Steps whose log may still grow. */
const LOG_ACTIVE = new Set(['claimed', 'waiting']);

/** Save the run's JSON document (the report the Markdown export renders). */
async function downloadJson(executionId: string, title: string): Promise<void> {
  const { data } = await api.GET('/v1/executions/{exe}/report', {
    params: { path: { exe: executionId } },
  });
  const blob = new Blob([`${JSON.stringify(data, null, 2)}\n`], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  try {
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = `${title.replace(/[^a-z0-9-_]+/gi, '-').toLowerCase() || executionId}.json`;
    anchor.click();
  } finally {
    setTimeout(() => URL.revokeObjectURL(url), 1_000);
  }
}

/** The Document view: the outline nests the sections; without one, document order. */
function DocumentTab({
  doc,
  compiler,
  names,
  documentAt,
}: {
  doc: RunDocument;
  compiler: CompilerBackend | undefined;
  names: DirectoryNames;
  documentAt: number;
}) {
  const outline = useRunOutline(doc.header, compiler);
  if (outline.kind === 'loading') {
    return (
      <div aria-busy="true">
        <p role="status" className="sr-only">
          Loading the document…
        </p>
        <Skeleton className="h-[60vh] w-full" />
      </div>
    );
  }
  return (
    <RunDocumentView
      doc={doc}
      outline={outline.kind === 'ready' ? outline.outline : null}
      names={names}
      documentAt={documentAt}
    />
  );
}

/** The Checklist view: onboarding-style checklist of runs with next up and sections. */
function ChecklistTab({
  doc,
  compiler,
  names,
  documentAt,
  executionId,
  me,
  caps,
  runPaused,
}: {
  doc: RunDocument;
  compiler: CompilerBackend | undefined;
  names: DirectoryNames;
  documentAt: number;
  executionId: string;
  me: Parameters<typeof StepActions>[0]['me'];
  caps: Parameters<typeof StepActions>[0]['capabilities'];
  runPaused: boolean;
}) {
  const outline = useRunOutline(doc.header, compiler);
  if (outline.kind === 'loading') {
    return (
      <div aria-busy="true">
        <p role="status" className="sr-only">
          Loading checklist…
        </p>
        <Skeleton className="h-[60vh] w-full" />
      </div>
    );
  }
  return (
    <RunChecklistView
      doc={doc}
      outline={outline.kind === 'ready' ? outline.outline : null}
      names={names}
      documentAt={documentAt}
      executionId={executionId}
      me={me}
      capabilities={caps}
      runPaused={runPaused}
    />
  );
}

function SectionWithLogs({
  projectId,
  traceProject,
  executionId,
  section,
  step,
  me,
  caps,
  index,
  isCurrent,
  documentAt,
  names,
  runPaused,
}: {
  projectId?: string | null;
  traceProject?: string | null;
  executionId: string;
  section: Parameters<typeof StepSection>[0]['section'];
  step: Parameters<typeof StepActions>[0]['step'];
  me: Parameters<typeof StepActions>[0]['me'];
  caps: Parameters<typeof StepActions>[0]['capabilities'];
  index: number;
  isCurrent: boolean;
  documentAt: number;
  names: DirectoryNames;
  runPaused: boolean;
}) {
  const [wantLogs, setWantLogs] = useState(false);
  // While this tab's claim form shows, its checklist is the one to use:
  // the section's read-only copy would list every item twice (#6).
  const [formShown, setFormShown] = useState(false);
  const logs = useStepLogs(section.step_run_id, wantLogs);
  const lines = useMemo(
    () => (logs.data?.pages ?? []).flatMap((page) => page.lines.map((entry) => entry.line)),
    [logs.data],
  );
  const expired = logs.error instanceof ApiError && logs.error.status === 410;
  const logError =
    logs.error && !expired
      ? `The log could not be loaded${logs.error instanceof ApiError ? ` (${logs.error.code})` : ''}.`
      : null;

  // U17: an open log of a step that is still working (or just finished)
  // re-reads its pages whenever a live event refreshes the document; log
  // reads sit outside the execution's query key, so nothing else would.
  const { refetch: refetchLogs } = logs;
  const lastStatus = useRef(section.status);
  useEffect(() => {
    const wasActive = LOG_ACTIVE.has(lastStatus.current);
    lastStatus.current = section.status;
    if (wantLogs && (wasActive || LOG_ACTIVE.has(section.status))) void refetchLogs();
    // Only a new document read should trigger this.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [documentAt]);
  return (
    <div>
      <StepSection
        section={section}
        stepNumber={index + 1}
        isCurrent={isCurrent}
        checklist={formShown ? [] : (step?.checklist ?? [])}
        logLines={expired ? ['Log history has expired for this step.'] : lines}
        isLoadingLogs={logs.isLoading}
        logError={logError}
        hasMoreLogs={Boolean(logs.hasNextPage)}
        onExpandLogs={() => setWantLogs(true)}
        onLoadMoreLogs={() => void logs.fetchNextPage()}
        nameOf={names.nameOf}
        teamNameOf={names.teamName}
      />
      <StepAssignment section={section} nameOf={names.nameOf} />
      <RunnerStep step={step} project={projectId ?? undefined} traceProject={traceProject} />
      <StepActions
        executionId={executionId}
        section={section}
        step={step}
        me={me}
        capabilities={caps}
        documentAt={documentAt}
        names={names}
        onFormShownChange={setFormShown}
        runPaused={runPaused}
        project={projectId ?? undefined}
      />
    </div>
  );
}

export interface ExecutionDocumentPageProps {
  deepLinkedStep?: string;
  executionId: string;
  /** UI-3: the view shown (`?view=`); Graph by default. */
  view?: RunView;
  onViewChange?: (view: RunView) => void;
  /** Test seam: the browser compiler (the worker by default). */
  compiler?: CompilerBackend;
}

export function ExecutionDocumentPage({
  executionId,
  view: viewProp,
  onViewChange,
  compiler,
  deepLinkedStep,
}: ExecutionDocumentPageProps) {
  const [ownView, setOwnView] = useState<RunView>(viewProp ?? 'graph');
  const view = viewProp ?? ownView;
  const setView = (next: RunView) => {
    setOwnView(next);
    onViewChange?.(next);
  };
  const documentQuery = useExecutionDocument(executionId);
  const me = useMe();
  const names = useDirectoryNames();
  const { projects } = useOrgContext();
  const navigate = useNavigate();
  const doc = documentQuery.data;
  const terminal = doc
    ? TERMINAL.has(doc.header.status) || doc.header.phase === 'completed'
    : false;
  const live = useLiveExecution(executionId, Boolean(doc) && !terminal);
  const control = useExecutionControl(executionId);
  const rerun = useRerun(executionId);
  const projectRole = useMyProjectRole(doc?.header.project_id, me.data?.user?.id);
  const [selected, setSelected] = useState<string | null>(null);
  const [controlError, setControlError] = useState<string | null>(null);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const [confirmSignal, setConfirmSignal] = useState<string | null>(null);
  const signal = useSignal(executionId);

  const currentStepId = useMemo(
    () =>
      // A container stays `ready` while its children run: the current step
      // is the open step inside it, not the container (walkthrough #4).
      doc?.sections.find(
        (section) => ACTIVE_STEP.has(section.status) && !isContainerSection(section),
      )?.step_run_id ?? null,
    [doc],
  );
  const selectedStepId = selected ?? deepLinkedStep ?? currentStepId;

  const linkedStepExists = doc?.sections.some((section) => section.step_run_id === deepLinkedStep);
  useEffect(() => {
    const step = selected ?? (linkedStepExists && deepLinkedStep);
    if (step && view === 'steps') {
      document.getElementById(step)?.scrollIntoView?.({ block: 'start', behavior: 'smooth' });
      if (!selected) focusStep(step);
    }
  }, [selected, deepLinkedStep, linkedStepExists, view]);

  const announcements = useStatusAnnouncements(doc);

  // WCAG 2.4.2: the tab names the run and its status.
  const runTitle = doc?.header.title;
  const runStatus = doc?.header.status;
  useEffect(() => {
    if (!runTitle || !runStatus) return undefined;
    const before = document.title;
    document.title = `${runTitle} (${statusWords(runStatus)}) · ${APP_TITLE}`;
    return () => {
      document.title = before;
    };
  }, [runTitle, runStatus]);

  // Hooks stay above the early returns below (rules of hooks).
  const caps = capabilities(me.data, projectRole);
  const claimStep = useClaimStep(executionId);
  // The first step that is the viewer's to act on now, if any.
  const yourTurn = (() => {
    if (!doc || !me.data?.user || doc.header.status === 'paused') return null;
    const byId = new Map(doc.header.steps.map((step) => [step.id, step]));
    const mine = (refs: { kind: string; reference: string }[] | undefined) =>
      (refs ?? []).some(
        (ref) =>
          (ref.kind === 'user' && ref.reference === me.data?.user?.id) ||
          (ref.kind === 'team' &&
            (me.data?.teams.some((t) => t.id === ref.reference || t.slug === ref.reference) ??
              false)),
      );
    for (const section of doc.sections) {
      if (isContainerSection(section)) continue;
      const step = byId.get(section.step_run_id);
      if (!step) continue;
      const heldByMe = Boolean(tabClaims.get(section.step_run_id));
      if (heldByMe) return { section, action: 'continue' as const };
      if (
        section.approval &&
        section.status === 'waiting' &&
        isEligibleToDecide(section.approval, me.data)
      ) {
        return { section, action: 'decide' as const };
      }
      if (
        section.status === 'ready' &&
        !section.approval &&
        caps.canAct &&
        (mine(step.assignees) || mine(step.eligible_recipients))
      ) {
        return { section, action: 'claim' as const };
      }
    }
    return null;
  })();

  if (documentQuery.isLoading) {
    return (
      <div aria-busy="true" className="grid gap-3">
        <p role="status" className="sr-only">
          Loading execution…
        </p>
        <Skeleton className="h-10 w-64" />
        <Skeleton className="h-48 w-full" />
      </div>
    );
  }
  if (documentQuery.isError || !doc) {
    const error = documentQuery.error;
    const apiError = error instanceof ApiError ? error : null;
    return (
      <ErrorState
        title={apiError?.status === 410 ? 'History expired' : 'Execution could not be loaded'}
        message={
          apiError?.status === 410
            ? 'This run’s history has expired and can no longer be shown.'
            : 'The execution document could not be loaded.'
        }
        code={apiError?.code}
        requestId={apiError?.requestId ?? undefined}
        onRetry={() => void documentQuery.refetch()}
      />
    );
  }

  const header = doc.header;
  const stepsById = new Map(header.steps.map((step) => [step.id, step]));
  const showControlError = (error: unknown) =>
    setControlError(error instanceof ApiError ? friendlyError(error.code) : 'That action failed.');
  const showPatchError = (error: unknown) =>
    setControlError(
      error instanceof ApiError ? controlErrorText(error.code) : 'That action failed.',
    );

  // Signals the run waits on now (wait steps), once each.
  const signals = [
    ...new Set(
      header.steps
        .filter((step) => step.status === 'waiting' && step.wait?.signal)
        .map((step) => step.wait?.signal as string),
    ),
  ];

  const controls = (
    <>
      {caps.canControl && (header.status === 'running' || header.status === 'waiting') ? (
        <Button
          variant="secondary"
          disabled={control.isPending}
          onClick={() => {
            setControlError(null);
            control.mutate('pause', { onError: showPatchError });
          }}
        >
          Pause
        </Button>
      ) : null}
      {caps.canControl && header.status === 'paused' ? (
        <Button
          variant="secondary"
          disabled={control.isPending}
          onClick={() => {
            setControlError(null);
            control.mutate('resume', { onError: showPatchError });
          }}
        >
          Resume
        </Button>
      ) : null}
      {caps.canControl && !terminal ? (
        <Button
          variant="secondary"
          disabled={control.isPending}
          onClick={() => setConfirmCancel(true)}
        >
          Cancel run
        </Button>
      ) : null}
      {caps.canAct && !terminal
        ? signals.map((name) => (
            <Button
              key={name}
              variant="secondary"
              disabled={signal.isPending}
              onClick={() => setConfirmSignal(name)}
            >
              <Radio className="size-3.5" aria-hidden="true" />
              Send {name}
            </Button>
          ))
        : null}
      {caps.canRerun && terminal ? (
        <Button
          variant="secondary"
          disabled={rerun.isPending}
          onClick={() => {
            setControlError(null);
            rerun.mutate(undefined, {
              onError: showControlError,
              onSuccess: (created) => {
                void navigate({ to: '/x/$exe', params: { exe: created.id } });
              },
            });
          }}
        >
          Rerun
        </Button>
      ) : null}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="secondary">
            <Download className="size-3.5" aria-hidden="true" />
            Export
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem
            onSelect={() => {
              setControlError(null);
              downloadReport(executionId, header.title, 'markdown').catch(showControlError);
            }}
          >
            Markdown (.md)
          </DropdownMenuItem>
          <DropdownMenuItem
            onSelect={() => {
              setControlError(null);
              downloadReport(executionId, header.title, 'junit').catch(showControlError);
            }}
          >
            JUnit XML (.xml)
          </DropdownMenuItem>
          <DropdownMenuItem
            onSelect={() => {
              setControlError(null);
              downloadReport(executionId, header.title, 'pdf').catch(showControlError);
            }}
          >
            PDF (.pdf)
          </DropdownMenuItem>
          <DropdownMenuItem
            onSelect={() => {
              setControlError(null);
              downloadJson(executionId, header.title).catch(showControlError);
            }}
          >
            JSON (.json)
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {caps.controlNeedsScope && !terminal ? (
        <p className="basis-full text-xs text-fg-muted">{CONTROL_SCOPE_HINT}</p>
      ) : null}
      <ConfirmDialog
        open={confirmCancel}
        onOpenChange={setConfirmCancel}
        title="Cancel this run?"
        description="Cancelling stops the run and cannot be undone."
        confirmLabel="Cancel run"
        destructive
        onConfirm={() => {
          setControlError(null);
          control.mutate('cancel', { onError: showPatchError });
        }}
      />
      <ConfirmDialog
        open={confirmSignal !== null}
        onOpenChange={(open) => !open && setConfirmSignal(null)}
        title={`Send the signal ${confirmSignal ?? ''}?`}
        description="The run continues as if the event it waits for happened. This cannot be undone."
        confirmLabel="Send signal"
        onConfirm={() => {
          const name = confirmSignal;
          setConfirmSignal(null);
          if (!name) return;
          setControlError(null);
          signal.mutate({ name }, { onError: showControlError });
        }}
      />
    </>
  );

  // A call step's child runbook: the runbook page resolves a slug (UI-2).
  const projectSlug = projects.find((p) => p.id === header.project_id)?.slug;
  const callHref = projectSlug
    ? (slug: string) => `/p/${projectSlug}/runbooks/${encodeURIComponent(slug)}`
    : undefined;
  const stepsView =
    doc.sections.length === 0 ? (
      // Nothing has run yet: say why instead of "Steps 0" over an empty
      // panel (walkthrough #8). A queued run waits for a concurrency slot.
      <EmptyState
        title={header.status === 'queued' ? 'Queued: waiting for a slot' : 'No steps yet'}
        description={
          header.status === 'queued'
            ? 'This run starts when a concurrency slot frees up. Its steps appear here as it runs.'
            : 'Steps appear here as the run reaches them.'
        }
      />
    ) : (
      <SplitPane
        leftWidth="md:w-64"
        // Desktop: the rail pins below the sticky top bar (h-12) while the
        // document scrolls, and scrolls on its own when it is taller than
        // the window (walkthrough #3). The frame must not clip (overflow
        // hidden would make it the sticky pane's scroller), and the divider
        // moves to the document so it runs the full height.
        className="overflow-visible"
        leftClassName="md:sticky md:top-14 md:self-start md:border-r-0"
        rightClassName="md:border-l md:border-border"
        left={
          <StepRail
            className="md:max-h-[calc(100dvh-8rem)]"
            steps={doc.sections.map((section) => {
              const step = stepsById.get(section.step_run_id);
              return {
                step_run_id: section.step_run_id,
                title: section.title,
                path: section.path,
                status: sectionDisplayStatus(section),
                verdict: section.verdict,
                needsAction:
                  ACTIVE_STEP.has(section.status) && !isContainerSection(section) && Boolean(step),
                followup: section.followup,
              };
            })}
            selectedStepRunId={selectedStepId}
            onSelect={setSelected}
            nameOf={names.nameOf}
            teamNameOf={names.teamName}
            onActivate={focusStep}
          />
        }
        right={
          <div>
            {doc.sections.map((section, index) => (
              <SectionWithLogs
                projectId={doc.header.project_id}
                traceProject={doc.trace_project}
                key={section.step_run_id}
                executionId={executionId}
                section={section}
                step={stepsById.get(section.step_run_id)}
                me={me.data}
                caps={caps}
                index={index}
                isCurrent={section.step_run_id === currentStepId}
                documentAt={documentQuery.dataUpdatedAt}
                names={names}
                runPaused={header.status === 'paused'}
              />
            ))}
          </div>
        }
      />
    );

  return (
    <div data-slot="run-page" className="grid min-w-0 grid-cols-[minmax(0,1fr)] gap-4">
      <RunHeader header={header} names={names} live={live} terminal={terminal} actions={controls} />
      {/* Live changes, read out politely and in batches (U11). */}
      <div role="log" aria-live="polite" aria-label="Run updates" className="sr-only">
        {announcements.map((entry) => (
          <p key={entry.id}>{entry.text}</p>
        ))}
      </div>
      {controlError ? <p role="alert">{controlError}</p> : null}
      <div className="min-w-0 [overflow-wrap:anywhere]">
        <UndeliveredNotice
          notifications={header.notifications_not_delivered}
          compact
          dismissKey={executionId}
        />
      </div>
      {header.status === 'queued' ? (
        <p
          role="note"
          className="rounded-md border border-border bg-surface-raised px-3 py-2 text-sm text-fg-muted"
        >
          Queued: this run starts when a concurrency slot frees up.
        </p>
      ) : null}
      {yourTurn ? (
        <section
          aria-label="Your turn"
          className="flex flex-wrap items-center gap-3 rounded-md border border-accent bg-accent-subtle px-3 py-2"
        >
          <p className="min-w-0 flex-1 text-sm">
            <strong className="font-medium">{yourTurn.section.title}</strong>{' '}
            {yourTurn.action === 'continue' ? 'is yours to finish.' : 'is waiting for you.'}
          </p>
          {yourTurn.action === 'claim' ? (
            <Button
              disabled={claimStep.isPending}
              onClick={() => {
                claimStep.mutate(
                  { stepRunId: yourTurn.section.step_run_id },
                  {
                    onSuccess: () => {
                      setView('steps');
                      setSelected(yourTurn.section.step_run_id);
                      focusStep(yourTurn.section.step_run_id);
                    },
                    onError: (error) =>
                      setControlError(
                        error instanceof ApiError
                          ? friendlyError(error.code)
                          : 'That claim failed.',
                      ),
                  },
                );
              }}
            >
              {claimStep.isPending ? 'Claiming…' : 'Claim'}
            </Button>
          ) : (
            <Button
              onClick={() => {
                setView('steps');
                setSelected(yourTurn.section.step_run_id);
                focusStep(yourTurn.section.step_run_id);
              }}
            >
              {yourTurn.action === 'decide' ? 'Review the approval' : 'Go to the step'}
            </Button>
          )}
        </section>
      ) : null}
      <Tabs value={view} onValueChange={(next) => setView(next as RunView)}>
        <TabsList aria-label="Run views" className="print:hidden">
          {RUN_VIEWS.map((value) => (
            <TabsTrigger key={value} value={value}>
              {RUN_VIEW_LABELS[value]}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="graph">
          <RunGraphView
            executionId={executionId}
            doc={doc}
            me={me.data}
            caps={caps}
            documentAt={documentQuery.dataUpdatedAt}
            names={names}
            {...(compiler ? { compiler } : {})}
            {...(callHref ? { callHref } : {})}
            onShowDocument={() => setView('document')}
          />
        </TabsContent>
        <TabsContent value="document">
          <DocumentTab
            doc={doc}
            compiler={compiler}
            names={names}
            documentAt={documentQuery.dataUpdatedAt}
          />
        </TabsContent>
        <TabsContent value="steps">{stepsView}</TabsContent>
        <TabsContent value="checklist">
          <ChecklistTab
            doc={doc}
            compiler={compiler}
            names={names}
            documentAt={documentQuery.dataUpdatedAt}
            executionId={executionId}
            me={me.data}
            caps={caps}
            runPaused={header.status === 'paused'}
          />
        </TabsContent>
      </Tabs>
    </div>
  );
}
