// ONB-VIEW: Checklist view of a run (?view=checklist).
// Designed for custom onboarding and human/agent collaborative runs.
// Shows overall progress at the top, a "Next up" block for active/waiting steps,
// and runbook top-level steps as sections (parallel groups marked "Any order",
// foreach items, taken switch branch, and inline actions). Print-friendly.
import { type ComponentType } from 'react';
import {
  AlertTriangle,
  CheckCircle2,
  CircleDot,
  CircleSlash,
  Clock,
  Hourglass,
  Play,
  SkipForward,
  XCircle,
} from 'lucide-react';
import type { Outline } from '../../ui/types';
import { cn } from '../../ui/primitives/cn';
import type { DirectoryNames } from '../context/names';
import type { RunDocument } from './overlay';
import { StepActions } from './StepActions';
import { FollowupTags, StepLabels } from '../../ui/status';
import {
  buildChecklistSections,
  computeChecklistProgress,
  extractLeafItems,
  formatTime,
  waitingOnLabel,
  whoCompleted,
  type ChecklistStepItem,
} from './checklist';

type Icon = ComponentType<{ className?: string; 'aria-hidden'?: boolean | 'true' }>;

const STATUS_ICON: Record<string, { icon: Icon; className: string; words: string }> = {
  pending: { icon: Clock, className: 'text-fg-muted', words: 'Pending' },
  ready: { icon: CircleDot, className: 'text-status-queued-fg', words: 'Ready' },
  running: { icon: Play, className: 'text-status-running-fg', words: 'Running' },
  claimed: { icon: Play, className: 'text-status-running-fg', words: 'Claimed' },
  waiting: { icon: Hourglass, className: 'text-status-waiting-fg', words: 'Waiting' },
  succeeded: { icon: CheckCircle2, className: 'text-status-succeeded-fg', words: 'Succeeded' },
  failed: { icon: XCircle, className: 'text-status-failed-fg', words: 'Failed' },
  skipped: { icon: SkipForward, className: 'text-status-skipped-fg', words: 'Skipped' },
  cancelled: { icon: CircleSlash, className: 'text-status-cancelled-fg', words: 'Cancelled' },
  timed_out: { icon: AlertTriangle, className: 'text-status-timed-out-fg', words: 'Timed out' },
  expired: { icon: AlertTriangle, className: 'text-status-expired-fg', words: 'Expired' },
};

const VERDICT_ICON: Record<
  'pass' | 'warn' | 'fail',
  { icon: Icon; className: string; words: string }
> = {
  pass: { icon: CheckCircle2, className: 'text-verdict-pass-fg', words: 'Pass' },
  warn: { icon: AlertTriangle, className: 'text-verdict-warn-fg', words: 'Warn' },
  fail: { icon: XCircle, className: 'text-verdict-fail-fg', words: 'Fail' },
};

function StatusGlyph({
  status,
  verdict,
  className,
}: {
  status: string;
  verdict?: 'pass' | 'warn' | 'fail' | null;
  className?: string;
}) {
  const entry =
    (verdict ? VERDICT_ICON[verdict] : undefined) ?? STATUS_ICON[status] ?? STATUS_ICON.pending!;
  const Glyph = entry.icon;
  return (
    <span data-status={status} data-verdict={verdict ?? undefined} className="inline-flex items-center">
      <Glyph className={cn('size-4 shrink-0', entry.className, className)} aria-hidden="true" />
      <span className="sr-only">{entry.words}: </span>
    </span>
  );
}

function ChecklistRow({
  item,
  names,
  executionId,
  me,
  capabilities,
  documentAt,
  runPaused,
}: {
  item: ChecklistStepItem;
  names: DirectoryNames;
  executionId: string;
  me: Parameters<typeof StepActions>[0]['me'];
  capabilities: Parameters<typeof StepActions>[0]['capabilities'];
  documentAt: number;
  runPaused: boolean;
}) {
  const isOpen = item.status === 'ready' || item.status === 'waiting' || item.status === 'claimed';
  const isDone = item.status === 'succeeded';
  const isFailed = item.status === 'failed' || item.status === 'timed_out' || item.status === 'expired';
  const isSkipped = item.status === 'skipped';
  const isCancelled = item.status === 'cancelled';

  return (
    <div
      data-slot="checklist-row"
      data-status={item.status}
      className={cn(
        'rounded-md border border-border/70 bg-surface p-3.5 transition-colors print:border-0 print:p-1',
        isOpen && 'border-border bg-surface shadow-xs',
        isDone && 'bg-surface/80',
      )}
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2.5 min-w-0">
          <StatusGlyph status={item.status} verdict={item.verdict} />
          <span
            className={cn(
              'text-sm font-medium leading-snug break-words',
              isDone ? 'text-fg' : 'text-fg',
              (isSkipped || isCancelled) && 'text-fg-muted line-through decoration-fg-muted/50',
            )}
          >
            {item.title}
          </span>
          <StepLabels labels={item.section?.labels} />
          <FollowupTags
            followup={item.section?.followup}
            nameOf={names.nameOf}
            teamNameOf={names.teamName}
          />
        </div>
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-fg-muted shrink-0">
          {isDone ? (
            <>
              {whoCompleted(item.section, names) ? (
                <span data-slot="who-completed">{whoCompleted(item.section, names)}</span>
              ) : null}
              {item.completedAtMs ? (
                <span data-slot="finished-time" className="font-mono text-2xs text-fg-muted">
                  {formatTime(item.completedAtMs)}
                </span>
              ) : null}
            </>
          ) : isOpen ? (
            <span data-slot="waiting-on" className="font-medium text-fg">
              {waitingOnLabel(item.step, item.section, item.node, names)}
            </span>
          ) : isSkipped ? (
            <span className="text-status-skipped-fg">Skipped</span>
          ) : isCancelled ? (
            <span className="text-status-cancelled-fg">Cancelled</span>
          ) : isFailed ? (
            <span className="text-status-failed-fg font-medium">Failed</span>
          ) : (
            <span>Pending</span>
          )}
        </div>
      </div>

      {item.section && item.step && isOpen ? (
        <div className="mt-2 pt-2 border-t border-border/40 print:hidden">
          <StepActions
            executionId={executionId}
            section={item.section}
            step={item.step}
            me={me}
            capabilities={capabilities}
            documentAt={documentAt}
            names={names}
            runPaused={runPaused}
          />
        </div>
      ) : null}
    </div>
  );
}

export interface RunChecklistViewProps {
  doc: RunDocument;
  outline: Outline | null;
  names: DirectoryNames;
  documentAt: number;
  executionId: string;
  me: Parameters<typeof StepActions>[0]['me'];
  capabilities: Parameters<typeof StepActions>[0]['capabilities'];
  runPaused: boolean;
}

export function RunChecklistView({
  doc,
  outline,
  names,
  documentAt,
  executionId,
  me,
  capabilities,
  runPaused,
}: RunChecklistViewProps) {
  const header = doc.header;
  const progress = computeChecklistProgress(doc, outline);
  const percent = progress.total > 0 ? Math.round((Math.min(progress.done, progress.total) / progress.total) * 100) : 0;
  const sections = buildChecklistSections(doc, outline);
  const allLeafItems = extractLeafItems(sections);
  const nextUpItems = allLeafItems.filter(
    (item) => item.status === 'ready' || item.status === 'waiting' || item.status === 'claimed',
  );

  return (
    <article
      data-slot="run-checklist"
      aria-label={`${header.title}, as a checklist`}
      className="w-full space-y-6 rounded-lg border border-border bg-surface px-5 py-7 shadow-card sm:px-10 sm:py-8 print:max-w-none print:border-0 print:p-0 print:shadow-none"
    >
      {/* Header and Progress */}
      <header className="border-b border-border pb-6 space-y-4">
        <div>
          <p className="text-xs font-medium uppercase tracking-wider text-fg-muted">
            Checklist view
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
          <h2 className="mt-1 text-2xl font-semibold leading-tight tracking-tight text-fg">
            {header.title}
          </h2>
        </div>

        {/* Progress bar at the top */}
        <div data-slot="checklist-progress" className="rounded-md border border-border bg-surface-raised p-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex items-center gap-3">
              <span className="text-sm font-semibold text-fg">
                {progress.done} of {progress.total} done
              </span>
              <div
                role="progressbar"
                aria-valuenow={progress.done}
                aria-valuemin={0}
                aria-valuemax={progress.total}
                aria-label={`${progress.done} of ${progress.total} done`}
                className="relative h-2 w-36 overflow-hidden rounded-full bg-hover"
              >
                <div
                  className="absolute inset-y-0 left-0 rounded-full bg-accent transition-[width] duration-300"
                  style={{ width: `${percent}%` }}
                />
              </div>
            </div>
            <div className="flex items-center gap-3 text-xs">
              {progress.skipped > 0 ? (
                <span className="text-status-skipped-fg font-medium">
                  {progress.skipped} skipped
                </span>
              ) : null}
              {progress.cancelled > 0 ? (
                <span className="text-status-cancelled-fg font-medium">
                  {progress.cancelled} cancelled
                </span>
              ) : null}
            </div>
          </div>
        </div>
      </header>

      {/* Next Up: steps ready or waiting now */}
      {nextUpItems.length > 0 ? (
        <section
          data-slot="next-up"
          aria-label="Next up"
          className="rounded-lg border border-border bg-surface-raised p-4 sm:p-5 print:border-0 print:p-0"
        >
          <h3 className="text-xs font-semibold uppercase tracking-wider text-fg-muted">
            Next up
          </h3>
          <div className="mt-3 space-y-2">
            {nextUpItems.map((item) => (
              <div
                key={`next-up-${item.path}`}
                data-slot="next-up-item"
                className="rounded-md border border-border bg-surface p-3"
              >
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div className="flex items-center gap-2">
                    <StatusGlyph status={item.status} verdict={item.verdict} />
                    <span className="text-sm font-medium text-fg">{item.title}</span>
                    <StepLabels labels={item.section?.labels} />
                    <FollowupTags
                      followup={item.section?.followup}
                      nameOf={names.nameOf}
                      teamNameOf={names.teamName}
                    />
                  </div>
                  <span data-slot="waiting-on" className="text-xs font-medium text-fg">
                    {waitingOnLabel(item.step, item.section, item.node, names)}
                  </span>
                </div>
                {item.section && item.step ? (
                  <div className="mt-2 pt-2 border-t border-border/40 print:hidden">
                    <StepActions
                      executionId={executionId}
                      section={item.section}
                      step={item.step}
                      me={me}
                      capabilities={capabilities}
                      documentAt={documentAt}
                      names={names}
                      runPaused={runPaused}
                    />
                  </div>
                ) : null}
              </div>
            ))}
          </div>
        </section>
      ) : null}

      {/* Sections: top-level steps in order */}
      <section data-slot="checklist-sections" aria-label="Run sections" className="space-y-6">
        <h3 className="sr-only">Run sections</h3>
        {sections.map((section, sIdx) => {
          if (section.kind === 'single') {
            return (
              <div key={section.item.path} data-slot="checklist-section" data-kind="single">
                <ChecklistRow
                  item={section.item}
                  names={names}
                  executionId={executionId}
                  me={me}
                  capabilities={capabilities}
                  documentAt={documentAt}
                  runPaused={runPaused}
                />
              </div>
            );
          }

          if (section.kind === 'parallel') {
            return (
              <div
                key={`parallel-${sIdx}-${section.title}`}
                data-slot="checklist-section"
                data-kind="parallel"
                className="rounded-lg border border-border bg-surface-raised/40 p-4 space-y-3"
              >
                <div className="flex items-center justify-between gap-2">
                  <h4 className="text-sm font-semibold text-fg">{section.title}</h4>
                  <span
                    data-slot="any-order-badge"
                    className="rounded bg-surface-raised border border-border/60 px-2 py-0.5 text-xs font-medium text-fg-muted"
                  >
                    Any order
                  </span>
                </div>
                <div className="space-y-2">
                  {section.items.map((item) => (
                    <ChecklistRow
                      key={item.path}
                      item={item}
                      names={names}
                      executionId={executionId}
                      me={me}
                      capabilities={capabilities}
                      documentAt={documentAt}
                      runPaused={runPaused}
                    />
                  ))}
                </div>
              </div>
            );
          }

          if (section.kind === 'foreach') {
            return (
              <div
                key={`foreach-${sIdx}-${section.title}`}
                data-slot="checklist-section"
                data-kind="foreach"
                className="rounded-lg border border-border bg-surface-raised/40 p-4 space-y-4"
              >
                <h4 className="text-sm font-semibold text-fg">{section.title}</h4>
                {section.items.map((it) => (
                  <div key={it.key} className="space-y-2">
                    <p className="text-xs font-medium uppercase tracking-wider text-fg-muted">
                      {it.label}
                    </p>
                    <div className="space-y-2">
                      {it.steps.map((item) => (
                        <ChecklistRow
                          key={item.path}
                          item={item}
                          names={names}
                          executionId={executionId}
                          me={me}
                          capabilities={capabilities}
                          documentAt={documentAt}
                          runPaused={runPaused}
                        />
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            );
          }

          if (section.kind === 'switch') {
            return (
              <div
                key={`switch-${sIdx}-${section.title}`}
                data-slot="checklist-section"
                data-kind="switch"
                className="rounded-lg border border-border bg-surface-raised/40 p-4 space-y-3"
              >
                <div className="flex items-center justify-between gap-2">
                  <h4 className="text-sm font-semibold text-fg">{section.title}</h4>
                  {section.branchLabel ? (
                    <span className="font-mono text-2xs text-fg-muted">
                      {section.branchLabel}
                    </span>
                  ) : null}
                </div>
                <div className="space-y-2">
                  {section.items.map((item) => (
                    <ChecklistRow
                      key={item.path}
                      item={item}
                      names={names}
                      executionId={executionId}
                      me={me}
                      capabilities={capabilities}
                      documentAt={documentAt}
                      runPaused={runPaused}
                    />
                  ))}
                </div>
              </div>
            );
          }

          if (section.kind === 'group') {
            return (
              <div
                key={`group-${sIdx}-${section.title}`}
                data-slot="checklist-section"
                data-kind="group"
                className="rounded-lg border border-border bg-surface-raised/40 p-4 space-y-3"
              >
                <h4 className="text-sm font-semibold text-fg">{section.title}</h4>
                <div className="space-y-2">
                  {section.items.map((item) => (
                    <ChecklistRow
                      key={item.path}
                      item={item}
                      names={names}
                      executionId={executionId}
                      me={me}
                      capabilities={capabilities}
                      documentAt={documentAt}
                      runPaused={runPaused}
                    />
                  ))}
                </div>
              </div>
            );
          }

          return null;
        })}
      </section>
    </article>
  );
}
