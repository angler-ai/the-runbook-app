// UI-3: the run as a document (the user's "reads like a Confluence doc").
// One paper page: the title, an outcome line and who ran it, then every
// step as a section in runbook order, nested as the runbook nests them,
// each with its status and verdict, who did it, the summary as prose,
// outputs as a small table, evidence links, the log (collapsed) and any
// approval as a callout. Skipped, untaken and not-yet-reached steps fold
// into one line each. It shows the same document the Markdown export
// renders (GET /v1/executions/{exe}/report): the same sections, fields
// and decisions, so screen and export agree. It prints as the page.
import type { ComponentType, ReactNode } from 'react';
import { Link } from '@tanstack/react-router';
import {
  AlertTriangle,
  CheckCircle2,
  CircleDot,
  CircleSlash,
  Clock,
  ExternalLink,
  Hourglass,
  Play,
  Radio,
  ShieldCheck,
  SkipForward,
  UserCheck,
  XCircle,
} from 'lucide-react';
import type { Outline } from '../../ui/types';
import { LogBlock } from '../../ui/execution';
import { cn } from '../../ui/primitives/cn';
import { RunStateChip } from '../../ui/graph';
import { FollowupTags, StepLabels } from '../../ui/status';
import { principalActor } from '../home/model';
import type { DirectoryNames } from '../context/names';
import { useLogView } from './logs';
import {
  actorOf,
  documentItems,
  outcomeOf,
  participants,
  runState,
  type DocItem,
  type RunDocument,
  type RunRecord,
} from './overlay';
import { EvidenceList, OutputsTable } from './RunStepPanel';
import { strictAssigneeLabel, takenOverText } from './takeOver';

type Icon = ComponentType<{ className?: string; 'aria-hidden'?: boolean | 'true' }>;

const STATUS_ICON: Record<string, { icon: Icon; className: string; words: string }> = {
  pending: { icon: Clock, className: 'text-fg-muted', words: 'Pending' },
  ready: { icon: CircleDot, className: 'text-status-queued-fg', words: 'Ready' },
  running: { icon: Play, className: 'text-status-running-fg', words: 'Running' },
  waiting: { icon: Hourglass, className: 'text-status-waiting-fg', words: 'Waiting' },
  succeeded: { icon: CheckCircle2, className: 'text-status-succeeded-fg', words: 'Succeeded' },
  failed: { icon: XCircle, className: 'text-status-failed-fg', words: 'Failed' },
  skipped: { icon: SkipForward, className: 'text-status-skipped-fg', words: 'Skipped' },
  cancelled: { icon: CircleSlash, className: 'text-status-cancelled-fg', words: 'Cancelled' },
  timed_out: { icon: AlertTriangle, className: 'text-status-timed-out-fg', words: 'Timed out' },
  expired: { icon: AlertTriangle, className: 'text-status-expired-fg', words: 'Expired' },
  queued: { icon: Clock, className: 'text-status-queued-fg', words: 'Queued' },
  paused: { icon: Hourglass, className: 'text-status-paused-fg', words: 'Paused' },
};

/** A check's verdict outranks its status: a check that ran and failed is a fail. */
const VERDICT_ICON: Record<
  'pass' | 'warn' | 'fail',
  { icon: Icon; className: string; words: string }
> = {
  pass: { icon: CheckCircle2, className: 'text-verdict-pass-fg', words: 'Pass' },
  warn: { icon: AlertTriangle, className: 'text-verdict-warn-fg', words: 'Warn' },
  fail: { icon: XCircle, className: 'text-verdict-fail-fg', words: 'Fail' },
};

function StatusIcon({
  status,
  verdict,
  className,
  slot,
}: {
  status: string;
  verdict?: 'pass' | 'warn' | 'fail' | null | undefined;
  className?: string;
  slot?: string;
}) {
  const entry =
    (verdict ? VERDICT_ICON[verdict] : undefined) ?? STATUS_ICON[status] ?? STATUS_ICON.pending!;
  const Glyph = entry.icon;
  return (
    <>
      <span
        data-slot={slot}
        data-icon={verdict ?? (STATUS_ICON[status] ? status : 'pending')}
        className="contents"
      >
        <Glyph className={cn('size-4 shrink-0', entry.className, className)} aria-hidden="true" />
      </span>
      <span className="sr-only">{entry.words}: </span>
    </>
  );
}

function dateTime(ms: number): string {
  return new Date(ms).toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function time(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function Callout({
  icon: Glyph,
  tone = 'neutral',
  children,
  slot,
}: {
  icon: Icon;
  tone?: 'neutral' | 'accent' | 'warning';
  children: ReactNode;
  slot?: string;
}) {
  return (
    <div
      data-slot={slot}
      className={cn(
        'flex gap-2.5 rounded-md border-l-[3px] px-3 py-2 text-sm',
        tone === 'accent' && 'border-accent bg-accent-subtle',
        tone === 'warning' && 'border-warning bg-status-waiting-bg',
        tone === 'neutral' && 'border-border-strong bg-surface-raised',
      )}
    >
      <Glyph className="mt-0.5 size-4 shrink-0 text-fg-muted" aria-hidden="true" />
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

const LOGGED_KINDS = new Set(['task']);

function StepBody({
  record,
  names,
  documentAt,
  printNote,
  executionId,
}: {
  record: RunRecord;
  names: DirectoryNames;
  documentAt: number;
  printNote: string;
  executionId?: string;
}) {
  const { section, step } = record;
  const logs = useLogView(section, documentAt);
  const actor = actorOf(section.actor, names.nameOf);
  const taken = takenOverText(section, names.nameOf);
  const strict = section.assignee?.strict ? strictAssigneeLabel(section, names) : null;
  const approval = section.approval;
  const waitSignal = section.status === 'waiting' ? step?.wait?.signal : undefined;
  const hasLog = LOGGED_KINDS.has(section.kind ?? 'task');
  // The section carries the call too (the export reads it there).
  const call = step?.call ?? section.call ?? null;
  const memory = (
    [
      ['Memory proposed', section.memory_proposed],
      ['Memory saved', section.memory_saved],
      ['Memory edited', section.memory_edited],
    ] as const
  ).flatMap(([label, writes]) => (writes ?? []).map((write) => ({ label, write })));
  return (
    <div className="grid gap-2.5">
      {actor || taken ? (
        <p data-slot="doc-actor" className="text-xs text-fg-muted">
          {actor ? (
            <>
              {section.status === 'claimed'
                ? 'In progress by '
                : section.status === 'skipped'
                  ? 'Skipped by '
                  : 'By '}
              <span className={cn('text-fg', actor.kind === 'agent' && 'font-mono text-2xs')}>
                {actor.kind === 'agent' ? `agent ${actor.label}` : actor.label}
              </span>
            </>
          ) : null}
          {step && step.attempt > 1 ? ` · attempt ${step.attempt}` : ''}
        </p>
      ) : null}
      <StepLabels labels={section.labels} />
      {section.followup ? (
        <p data-slot="doc-followup" className="flex flex-wrap items-center gap-1">
          <FollowupTags
            followup={section.followup}
            nameOf={names.nameOf}
            teamNameOf={names.teamName}
          />
        </p>
      ) : null}
      {strict ? (
        <p data-slot="doc-assignee" className="text-xs text-fg-muted">
          Assigned to <span className="text-fg">{strict}</span> (strict)
          {section.assignee?.routing_error ? ' · nobody eligible can claim it yet' : ''}
        </p>
      ) : null}
      {taken ? (
        <Callout icon={UserCheck} tone="accent" slot="doc-takeover">
          {taken}.
        </Callout>
      ) : null}
      {section.summary ? (
        <p
          data-slot="doc-summary"
          className="whitespace-pre-wrap text-[15px] leading-relaxed text-fg"
        >
          {section.summary}
        </p>
      ) : null}
      {Object.keys(section.outputs).length > 0 ? (
        <div className="max-w-xl rounded-md border border-border px-3 py-1">
          <OutputsTable outputs={section.outputs} nameOf={names.nameOf} />
        </div>
      ) : null}
      {section.evidence.length > 0 ? (
        <EvidenceList evidence={section.evidence} executionId={executionId} />
      ) : null}
      {approval ? (
        <Callout
          icon={ShieldCheck}
          tone={
            approval.decisions.length === 0 && section.status !== 'succeeded'
              ? 'warning'
              : 'neutral'
          }
          slot="doc-approval"
        >
          <p className="font-medium">{approval.prompt}</p>
          {approval.blocked_reason === 'approval.review_missing_report' ? (
            <p className="mt-0.5 text-sm text-fg-muted">
              Work in this approval’s review scope timed out. A project admin can supply the missing
              result.
            </p>
          ) : approval.blocked_reason === 'approval.unknown_service_contributor' ? (
            <p className="mt-0.5 text-sm text-fg-muted">
              A service token contributor could not be identified. A named approver without project
              admin rights must review.
            </p>
          ) : approval.blocked_reason === 'approval.no_independent_approver' ? (
            <p className="mt-0.5 text-sm text-fg-muted">
              All listed approvers contributed to the reviewed work, directly or through an agent or
              service token.
            </p>
          ) : approval.blocked_reason === 'approval.independent_review_required' ? (
            <p className="mt-0.5 text-sm text-fg-muted">
              The automatic decision was withheld. This gate requires independent review.
            </p>
          ) : approval.decisions.length > 0 ? (
            <ul className="mt-1 grid gap-0.5 text-sm">
              {approval.decisions.map((d, i) => (
                <li key={i}>
                  {d.by === 'human'
                    ? names.nameOf(d.user_id)
                    : d.by === 'agent'
                      ? `Agent ${d.agent_name}${d.model ? ` (${d.model})` : ''}`
                      : `System policy (${d.policy})`}{' '}
                  chose <strong className="font-semibold">{d.option}</strong>
                  {'comment' in d && d.comment ? `: “${d.comment}”` : ''}
                </li>
              ))}
            </ul>
          ) : (
            <p className="mt-0.5 text-sm text-fg-muted">
              Waiting for a decision
              {approval.eligible_recipients.length > 0
                ? ` from ${approval.eligible_recipients
                    .map((r) =>
                      r.kind === 'user'
                        ? names.nameOf(r.reference)
                        : `team ${names.teamName(r.reference)}`,
                    )
                    .join(', ')}`
                : ''}
              .
            </p>
          )}
        </Callout>
      ) : null}
      {waitSignal ? (
        <Callout icon={Radio} tone="warning" slot="doc-signal">
          Waiting for the signal <code className="font-mono text-[0.92em]">{waitSignal}</code>
          {step?.wait?.until_ms ? `, until ${dateTime(step.wait.until_ms)}` : ''}.
        </Callout>
      ) : null}
      {call?.started ? (
        <p className="text-sm">
          <Link
            to="/x/$exe"
            params={{ exe: call.child_execution_id }}
            className="inline-flex items-center gap-1 font-medium text-accent underline-offset-2 hover:underline"
          >
            Open the child run
            <ExternalLink className="size-3.5" aria-hidden="true" />
          </Link>
          {call.error ? <span className="text-fg-muted"> ({call.error})</span> : null}
        </p>
      ) : call?.error ? (
        <p data-slot="doc-call-refused" className="text-sm text-fg-muted">
          The child run did not start (<code className="font-mono text-[0.92em]">{call.error}</code>
          ).
        </p>
      ) : null}
      {memory.length > 0 ? (
        <ul data-slot="doc-memory" className="grid gap-0.5 text-xs text-fg-muted">
          {memory.map(({ label, write }, i) => (
            <li key={`${label}-${write.memory_id}-${i}`}>
              {label}: <code className="font-mono text-fg">{write.memory_id}</code>
              {write.dropped ? ` (dropped: ${write.dropped})` : ''}
            </li>
          ))}
        </ul>
      ) : null}
      {hasLog && section.status !== 'pending' ? (
        <LogBlock
          lines={logs.lines}
          loading={logs.loading}
          error={logs.error}
          hasMore={logs.hasMore}
          onLoadMore={logs.loadMore}
          onOpen={logs.open}
          onCopyAll={logs.copyAll}
          printNote={printNote}
        />
      ) : null}
    </div>
  );
}

const FOLD_WORDS: Record<Extract<DocItem, { kind: 'folded' }>['reason'], string> = {
  skipped: 'Skipped',
  untaken: 'Not taken',
  upcoming: 'Not started yet',
  unreached: 'Did not run',
  empty: 'No items',
};

function Items({
  items,
  names,
  documentAt,
  printNote,
  executionId,
}: {
  items: DocItem[];
  names: DirectoryNames;
  documentAt: number;
  printNote: string;
  executionId?: string;
}) {
  return (
    <>
      {items.map((item, i) => {
        if (item.kind === 'folded') {
          return (
            <p
              key={`fold-${i}`}
              data-slot="doc-folded"
              data-reason={item.reason}
              className="flex items-baseline gap-2 py-1.5 text-sm text-fg-muted"
            >
              {item.reason === 'skipped' ? (
                <SkipForward className="size-3.5 shrink-0 translate-y-0.5" aria-hidden="true" />
              ) : (
                <CircleSlash className="size-3.5 shrink-0 translate-y-0.5" aria-hidden="true" />
              )}
              <span>
                <span className="font-medium">{FOLD_WORDS[item.reason]}</span>
                {item.branch ? (
                  <>
                    {' '}
                    (
                    {item.branch === 'default' ? (
                      'default'
                    ) : (
                      <code className="font-mono text-[0.92em]">{item.branch}</code>
                    )}
                    )
                  </>
                ) : null}
                : {item.titles.join(', ')}
              </span>
            </p>
          );
        }
        if (item.kind === 'iteration') {
          return (
            <div key={`it-${item.key}-${i}`} data-slot="doc-iteration" className="mt-3">
              <p className="text-xs font-semibold uppercase tracking-wider text-fg-muted">
                {item.label}
              </p>
              <div className="mt-1 border-l border-border pl-4">
                <Items
                  items={item.children}
                  names={names}
                  documentAt={documentAt}
                  printNote={printNote}
                  executionId={executionId}
                />
              </div>
            </div>
          );
        }
        return (
          <StepItem
            key={item.record.section.step_run_id}
            item={item}
            names={names}
            documentAt={documentAt}
            printNote={printNote}
            executionId={executionId}
          />
        );
      })}
    </>
  );
}

function StepItem({
  item,
  names,
  documentAt,
  printNote,
  executionId,
}: {
  item: Extract<DocItem, { kind: 'step' }>;
  names: DirectoryNames;
  documentAt: number;
  printNote: string;
  executionId?: string;
}) {
  const { section } = item.record;
  const state = runState(section);
  const Heading = item.depth === 0 ? 'h3' : 'h4';
  return (
    <section
      data-slot="doc-step"
      data-step={item.record.path}
      data-status={section.status}
      className={cn(
        'break-inside-avoid-page',
        item.depth === 0
          ? 'border-t border-border pt-5 mt-5 first:mt-0 first:border-t-0 first:pt-0'
          : 'mt-4',
      )}
    >
      <div className="flex items-start gap-2">
        <StatusIcon
          status={state}
          verdict={section.verdict}
          slot="doc-step-icon"
          className="mt-[3px]"
        />
        <Heading
          className={cn(
            'min-w-0 flex-1 font-semibold leading-snug text-fg',
            item.depth === 0 ? 'text-lg' : 'text-base',
          )}
        >
          {section.title || item.record.path}
        </Heading>
        {section.verdict ? (
          <RunStateChip state={state} label="" verdict={section.verdict} className="mt-0.5" />
        ) : null}
        {section.completed_at_ms ? (
          <span className="mt-0.5 shrink-0 font-mono text-2xs text-fg-muted">
            {time(section.completed_at_ms)}
          </span>
        ) : null}
      </div>
      <div className="mt-2 pl-6">
        <StepBody
          record={item.record}
          names={names}
          documentAt={documentAt}
          printNote={printNote}
          executionId={executionId}
        />
        {item.children.length > 0 ? (
          <div className="mt-2 border-l border-border pl-4">
            <Items
              items={item.children}
              names={names}
              documentAt={documentAt}
              printNote={printNote}
              executionId={executionId}
            />
          </div>
        ) : null}
      </div>
    </section>
  );
}

export interface RunDocumentViewProps {
  doc: RunDocument;
  outline: Outline | null;
  names: DirectoryNames;
  documentAt: number;
  /** Now, for an open run's elapsed time (a prop so tests are stable). */
  now?: number;
}

export function RunDocumentView({ doc, outline, names, documentAt, now }: RunDocumentViewProps) {
  const header = doc.header;
  const outcome = outcomeOf(doc, now ?? documentAt);
  const items = documentItems(outline, doc);
  const people = participants(doc, names.nameOf);
  const starter = header.created_by ? principalActor(header.created_by, names.nameOf) : null;
  const started = header.started_at_ms ?? header.created_at_ms;
  const inputs = Object.entries(header.inputs);
  // A closed log prints as where to read it (a printout cannot open it).
  const origin = typeof window === 'undefined' ? '' : window.location.origin;
  const runUrl = `${header.web_url || `${origin}/x/${header.id}`}?view=document`;
  const printNote = `Log not printed: open it on the run page to print it (${runUrl}).`;
  return (
    <article
      data-slot="run-document"
      aria-label={`${header.title}, as a document`}
      className="w-full rounded-lg border border-border bg-surface px-5 py-7 shadow-card sm:px-12 sm:py-10 print:max-w-none print:border-0 print:p-0 print:shadow-none"
    >
      <header className="border-b border-border pb-6">
        <p className="text-xs font-medium uppercase tracking-wider text-fg-muted">
          Run report
          {header.runbook_slug ? (
            <>
              {' · '}
              <span className="font-mono normal-case tracking-normal">
                {header.runbook_slug}
                {header.runbook_version != null ? ` v${header.runbook_version}` : ' (preview)'}
              </span>
            </>
          ) : null}
        </p>
        <h2 className="mt-2 text-2xl font-semibold leading-tight tracking-tight text-fg">
          {header.title}
        </h2>
        <p
          data-slot="doc-outcome"
          className="mt-3 flex flex-wrap items-center gap-x-2 gap-y-1 text-[15px] text-fg"
        >
          <StatusIcon status={header.status} />
          <span className="font-semibold">{outcome.headline}</span>
          {outcome.checks ? (
            <>
              <span aria-hidden="true" className="text-fg-muted">
                ·
              </span>
              <span>{outcome.checks}</span>
            </>
          ) : null}
        </p>
        <p data-slot="doc-who" className="mt-2 text-sm text-fg-muted">
          {starter ? (
            <>
              Started by <span className="text-fg">{starter.label}</span>
            </>
          ) : (
            'Started'
          )}
          {started ? ` on ${dateTime(started)}` : ''}
          {people.length > 0 ? (
            <>
              {'. Worked on by '}
              <span className="text-fg">{people.join(', ')}</span>
            </>
          ) : null}
          .
        </p>
        {header.superseded_by ? (
          <p data-slot="doc-superseded" className="mt-2 text-sm text-fg">
            Cancelled: replaced by{' '}
            <Link
              to="/x/$exe"
              params={{ exe: header.superseded_by.execution_id }}
              className="font-mono text-xs text-accent underline-offset-2 hover:underline"
            >
              {header.superseded_by.execution_id}
            </Link>
            , started by {principalActor(header.superseded_by.started_by, names.nameOf).label}
          </p>
        ) : null}
        {inputs.length > 0 ? (
          <dl className="mt-4 grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-sm">
            {inputs.map(([key, value]) => (
              <div key={key} className="contents">
                <dt className="font-mono text-xs leading-6 text-fg-muted">{key}</dt>
                <dd className="min-w-0 break-words font-mono text-xs leading-6 text-fg">
                  {typeof value === 'object' && value !== null
                    ? JSON.stringify(value)
                    : String(value)}
                </dd>
              </div>
            ))}
          </dl>
        ) : null}
      </header>
      <div className="pt-6">
        {items.length === 0 ? (
          <p className="text-sm text-fg-muted">
            {header.status === 'queued'
              ? 'This run is queued: it starts when a concurrency slot frees up.'
              : 'No steps have run yet.'}
          </p>
        ) : (
          <Items
            items={items}
            names={names}
            documentAt={documentAt}
            printNote={printNote}
            executionId={header.id}
          />
        )}
      </div>
    </article>
  );
}
