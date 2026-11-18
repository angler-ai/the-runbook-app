import { useState } from 'react';
import type { components } from '../../api/schema';
import { Button } from '../primitives/button';
import { cn } from '../primitives/cn';
import { ActorBadge } from '../status/ActorBadge';
import { AmendedTag } from '../status/AmendedTag';
import { MonoRef } from '../status/MonoRef';
import { StaleTag } from '../status/StaleTag';
import { StatusPill } from '../status/StatusPill';
import { FollowupTags, type TeamNameOf } from '../status/FollowupTags';
import { VerdictTag } from '../status/VerdictTag';
import { Check, ChevronDown, ChevronRight, Square } from 'lucide-react';
import { isUserId, shortId, type NameOf } from './names';

export type StepSectionType = components['schemas']['StepSection'];
export type ChecklistTick = components['schemas']['ChecklistTick'];

export interface StepSectionProps {
  section: StepSectionType;
  stepNumber?: number | string;
  isCurrent?: boolean;
  checklist?: ChecklistTick[];
  logLines?: string[];
  isLogsExpanded?: boolean;
  isLoadingLogs?: boolean;
  /** A failed log read, shown instead of "No logs recorded". Plain text. */
  logError?: string | null;
  hasMoreLogs?: boolean;
  onExpandLogs?: () => void;
  onLoadMoreLogs?: () => void;
  /** Resolves user ids to names (the org directory); defaults to a short id. */
  nameOf?: NameOf;
  /** Resolves team references to slugs; defaults to the reference as sent. */
  teamNameOf?: TeamNameOf;
  className?: string;
}

/** Steps that hold other steps: the engine reports one as `ready` while its body runs. */
const CONTAINER_KINDS = new Set(['group', 'parallel', 'foreach', 'loop', 'switch']);

/**
 * The status to show for a section (walkthrough #4). The engine keeps a
 * container (switch, loop, foreach, parallel, group) `ready` for as long
 * as its children are open, which reads as "nothing has started"; it is
 * running. Every other status shows as the engine reports it.
 */
export function sectionDisplayStatus(
  section: Pick<StepSectionType, 'kind' | 'status'>,
): StepSectionType['status'] | 'running' {
  return section.status === 'ready' && section.kind && CONTAINER_KINDS.has(section.kind)
    ? 'running'
    : section.status;
}

/** Whether a section is a container step (it holds other steps). */
export function isContainerSection(section: Pick<StepSectionType, 'kind'>): boolean {
  return Boolean(section.kind && CONTAINER_KINDS.has(section.kind));
}

/** The id of a step section's heading, the page's focus target for that step. */
export function stepHeadingId(stepRunId: string): string {
  return `${stepRunId}-heading`;
}

function isValidHttps(url: string): boolean {
  try {
    const parsed = new URL(url);
    return parsed.protocol === 'https:';
  } catch {
    return false;
  }
}

export function StepSection({
  section,
  stepNumber,
  isCurrent = false,
  checklist = [],
  logLines = [],
  isLogsExpanded,
  isLoadingLogs = false,
  logError = null,
  hasMoreLogs = false,
  onExpandLogs,
  onLoadMoreLogs,
  nameOf = shortId,
  teamNameOf,
  className,
}: StepSectionProps) {
  const [logsOpen, setLogsOpen] = useState(isLogsExpanded ?? false);

  const toggleLogs = () => {
    const next = !logsOpen;
    setLogsOpen(next);
    if (next) {
      onExpandLogs?.();
    }
  };

  const outputEntries = section.outputs ? Object.entries(section.outputs) : [];
  const memoryReads = section.memory_read ?? [];
  const evidenceList = section.evidence ?? [];
  const amendments = section.amendments ?? [];

  return (
    <section
      data-slot="step-section"
      data-step-id={section.step_run_id}
      id={section.step_run_id}
      className={cn(
        'border-t border-border py-6 scroll-mt-14 flex flex-col gap-4',
        isCurrent && 'bg-accent/[0.02]',
        className,
      )}
    >
      {/* Title bar */}
      <div className="flex flex-col sm:flex-row sm:items-baseline sm:justify-between gap-2">
        <div className="flex items-center gap-2 flex-wrap">
          {/* Focus target after claim, report, release and rail selection (U11). */}
          <h2
            id={stepHeadingId(section.step_run_id)}
            tabIndex={-1}
            className="text-base font-semibold text-fg tracking-tight rounded-sm focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
          >
            {stepNumber !== undefined && (
              <span className="mr-1.5 text-fg-muted">{stepNumber}.</span>
            )}{' '}
            {section.title}
          </h2>
          {section.stale && <StaleTag />}
          {section.amended && <AmendedTag />}
          <FollowupTags followup={section.followup} nameOf={nameOf} teamNameOf={teamNameOf} />
        </div>

        {section.verdict !== undefined && <VerdictTag verdict={section.verdict} />}
      </div>

      {/* Actor line and metadata */}
      <div className="flex flex-wrap items-center gap-3 text-xs text-fg-muted">
        <StatusPill status={sectionDisplayStatus(section)} />

        {section.actor && (
          <>
            <span className="text-border" aria-hidden="true">
              ·
            </span>
            <ActorBadge
              actor={section.actor}
              name={section.actor.kind === 'human' ? nameOf(section.actor.user_id) : undefined}
            />
          </>
        )}

        {section.path && (
          <>
            <span className="text-border" aria-hidden="true">
              ·
            </span>
            <MonoRef className="text-2xs text-fg-muted">{section.path}</MonoRef>
          </>
        )}
      </div>

      {/* Summary with whitespace-pre-wrap for literal line breaks */}
      {section.summary && (
        <div
          data-slot="step-summary"
          className="text-xs text-fg leading-relaxed whitespace-pre-wrap"
        >
          {section.summary}
        </div>
      )}

      {/* Checklist (read-only) */}
      {checklist.length > 0 && (
        <div data-slot="step-checklist" className="flex flex-col gap-1.5 pt-1">
          {checklist.map((item, idx) => (
            <div key={idx} className="flex items-center gap-2 text-xs text-fg">
              {item.completed ? (
                <Check className="size-3.5 text-status-succeeded-fg shrink-0" aria-hidden="true" />
              ) : (
                <Square className="size-3.5 text-fg-muted shrink-0" aria-hidden="true" />
              )}
              <span className={cn(item.completed && 'text-fg-muted')}>{item.title}</span>
            </div>
          ))}
        </div>
      )}

      {/* Outputs table */}
      {outputEntries.length > 0 && (
        <div
          data-slot="step-outputs"
          className="border border-border rounded-md bg-surface-raised overflow-hidden text-xs"
        >
          <div className="px-3 py-1.5 bg-surface border-b border-border text-2xs font-semibold text-fg-muted uppercase tracking-wider">
            Outputs
          </div>
          <div className="divide-y divide-border">
            {outputEntries.map(([key, val]) => {
              const isObj = typeof val === 'object' && val !== null;
              return (
                <div key={key} className="px-3 py-2 flex flex-col sm:flex-row sm:items-start gap-2">
                  <span className="font-mono text-2xs text-fg-muted shrink-0 min-w-28">{key}</span>
                  <div className="flex-1 min-w-0 font-mono text-xs text-fg">
                    {isObj ? (
                      <pre className="p-2 rounded bg-surface border border-border text-2xs whitespace-pre-wrap overflow-x-auto">
                        {JSON.stringify(val, null, 2)}
                      </pre>
                    ) : (
                      // A user id (an approval's decided_by, say) reads as a name.
                      <span title={isUserId(val) ? val : undefined}>
                        {isUserId(val) ? nameOf(val) : String(val)}
                      </span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Evidence links (HTTPS only) */}
      {evidenceList.length > 0 && (
        <div data-slot="step-evidence" className="flex flex-col gap-1.5 text-xs">
          <span className="text-2xs font-semibold uppercase tracking-wider text-fg-muted">
            Evidence
          </span>
          <ul className="flex flex-col gap-1">
            {evidenceList.map((item, idx) => {
              const isHttps = isValidHttps(item);
              return (
                <li key={idx} className="truncate">
                  {isHttps ? (
                    <a
                      href={item}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-accent underline font-mono text-xs hover:text-accent-hover break-all"
                    >
                      {item}
                    </a>
                  ) : (
                    <span className="font-mono text-xs text-fg-muted break-all">{item}</span>
                  )}
                </li>
              );
            })}
          </ul>
        </div>
      )}

      {/* Memory read lines */}
      {memoryReads.length > 0 && (
        <div data-slot="step-memory" className="flex flex-col gap-1 text-2xs text-fg-muted">
          {memoryReads.map((mem, idx) => (
            <div key={idx} className="flex items-center gap-1.5">
              <span>Used memory:</span>
              <MonoRef>{mem.memory_id}</MonoRef>
              <span>(rev {mem.revision})</span>
            </div>
          ))}
        </div>
      )}

      {/* Inline amendments */}
      {amendments.length > 0 && (
        <div
          data-slot="step-amendments"
          className="p-3 border border-dashed border-amended-border bg-amended-bg/30 rounded-md text-xs flex flex-col gap-2"
        >
          <div className="flex items-center gap-2">
            <AmendedTag />
            <span className="font-medium text-fg">Amendment History</span>
          </div>

          {amendments.map((amendment, idx) => (
            <div key={idx} className="flex flex-col gap-1.5 border-t border-amended-border/30 pt-2">
              <div className="flex items-center gap-2 text-2xs text-fg-muted">
                <span>By:</span>
                <ActorBadge
                  actor={amendment.by}
                  name={amendment.by.kind === 'human' ? nameOf(amendment.by.user_id) : undefined}
                />
              </div>
              <p className="text-xs text-fg italic">"{amendment.reason}"</p>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-3 mt-1 text-2xs">
                <div className="p-2 rounded bg-surface border border-border">
                  <span className="font-semibold text-fg-muted block mb-1">Original:</span>
                  <div>Status: {amendment.from.status}</div>
                  {amendment.from.verdict && <div>Verdict: {amendment.from.verdict}</div>}
                  {amendment.from.summary && (
                    <div className="italic mt-1">"{amendment.from.summary}"</div>
                  )}
                </div>
                <div className="p-2 rounded bg-surface border border-border">
                  <span className="font-semibold text-fg block mb-1">Amended to:</span>
                  <div>Status: {amendment.to.status}</div>
                  {amendment.to.verdict && <div>Verdict: {amendment.to.verdict}</div>}
                  {amendment.to.summary && (
                    <div className="italic mt-1">"{amendment.to.summary}"</div>
                  )}
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Collapsible LogBlock */}
      <div data-slot="step-log-block" className="pt-1">
        <button
          type="button"
          onClick={toggleLogs}
          aria-expanded={logsOpen}
          className="inline-flex items-center gap-1.5 text-xs font-mono text-fg-muted hover:text-fg focus-visible:outline-2 focus-visible:outline-accent select-none"
        >
          {logsOpen ? (
            <ChevronDown className="size-3.5 shrink-0" aria-hidden="true" />
          ) : (
            <ChevronRight className="size-3.5 shrink-0" aria-hidden="true" />
          )}
          <span>{logsOpen ? 'Hide log' : 'Show log'}</span>
          {logLines.length > 0 && <span>({logLines.length} lines)</span>}
        </button>

        {logsOpen && (
          <div className="mt-2.5 p-3 rounded-md bg-canvas border border-border overflow-x-auto text-xs font-mono leading-5">
            {isLoadingLogs && <p className="text-fg-muted">Loading logs...</p>}

            {logError && (
              <p role="alert" className="text-fg">
                {logError}
              </p>
            )}

            {!isLoadingLogs && !logError && logLines.length === 0 && (
              <p className="text-fg-muted">No logs recorded for this step.</p>
            )}

            {logLines.length > 0 && (
              <div className="flex flex-col">
                {logLines.map((line, idx) => (
                  <div key={idx} className="flex hover:bg-surface-raised/40">
                    <span className="w-8 shrink-0 text-right pr-3 text-fg-muted select-none border-r border-border mr-3">
                      {idx + 1}
                    </span>
                    <span className="flex-1 whitespace-pre-wrap text-fg break-all">{line}</span>
                  </div>
                ))}
              </div>
            )}

            {hasMoreLogs && onLoadMoreLogs && (
              <div className="pt-3 border-t border-border mt-3">
                <Button variant="secondary" size="sm" onClick={onLoadMoreLogs}>
                  Load more logs
                </Button>
              </div>
            )}
          </div>
        )}
      </div>
    </section>
  );
}
