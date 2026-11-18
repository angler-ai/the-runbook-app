import { useEffect, useRef, type KeyboardEvent } from 'react';
import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { FollowupTags, type StepFollowupView, type TeamNameOf } from '../status/FollowupTags';
import type { NameOf } from './names';

export type StepStatus = components['schemas']['StepStatus'];
type ExecutionStatus = components['schemas']['ExecutionStatus'];
export type Verdict = components['schemas']['Verdict'];

export interface StepRailItem {
  step_run_id: string;
  title: string;
  path?: string;
  status: StepStatus | ExecutionStatus;
  verdict?: Verdict | null;
  needsAction?: boolean;
  followup?: StepFollowupView | null;
}

export interface StepRailProps {
  steps: StepRailItem[];
  selectedStepRunId?: string | null;
  /** The highlighted step changed (click, Enter/Space, or arrow keys). */
  onSelect?: (stepRunId: string) => void;
  /** The person chose a step (click, Enter or Space): go to it. Arrow keys only select. */
  onActivate?: (stepRunId: string) => void;
  nameOf?: NameOf;
  teamNameOf?: TeamNameOf;
  className?: string;
  ariaLabel?: string;
}

const statusDotColors: Record<string, string> = {
  succeeded: 'bg-status-succeeded-fg',
  failed: 'bg-status-failed-fg',
  running: 'bg-status-running-fg animate-pulse',
  waiting: 'bg-status-waiting-fg',
  paused: 'bg-status-paused-fg',
  claimed: 'bg-status-running-fg',
  ready: 'bg-status-queued-fg',
  pending: 'bg-status-queued-fg/60',
  skipped: 'bg-status-skipped-fg',
  timed_out: 'bg-status-timed-out-fg',
  expired: 'bg-status-expired-fg',
  cancelled: 'bg-status-cancelled-fg',
};

export function StepRail({
  steps,
  selectedStepRunId,
  onSelect,
  onActivate,
  nameOf,
  teamNameOf,
  className,
  ariaLabel = 'Steps navigation',
}: StepRailProps) {
  const activate = (stepRunId: string) => {
    onSelect?.(stepRunId);
    onActivate?.(stepRunId);
  };
  // Roving tabindex: one Tab stop for the rail, arrows move within it.
  const tabStop = steps.some((step) => step.step_run_id === selectedStepRunId)
    ? selectedStepRunId
    : steps[0]?.step_run_id;
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const listRef = useRef<HTMLDivElement>(null);

  // Keep the selected (current) step in view inside the rail's own list
  // when the rail is height-bounded (walkthrough #3). Only the list
  // scrolls: scrollIntoView could move the page under the reader.
  const selectedIndex = steps.findIndex((step) => step.step_run_id === selectedStepRunId);
  useEffect(() => {
    const list = listRef.current;
    const item = itemRefs.current[selectedIndex];
    if (!list || !item || list.scrollHeight <= list.clientHeight) return;
    const box = list.getBoundingClientRect();
    const rect = item.getBoundingClientRect();
    if (rect.top < box.top) list.scrollTop -= box.top - rect.top;
    else if (rect.bottom > box.bottom) list.scrollTop += rect.bottom - box.bottom;
  }, [selectedIndex]);

  const handleKeyDown = (e: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let nextIndex = index;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      nextIndex = (index + 1) % steps.length;
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      nextIndex = (index - 1 + steps.length) % steps.length;
    } else if (e.key === 'Home') {
      e.preventDefault();
      nextIndex = 0;
    } else if (e.key === 'End') {
      e.preventDefault();
      nextIndex = steps.length - 1;
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      const current = steps[index];
      if (current) activate(current.step_run_id);
      return;
    } else {
      return;
    }

    itemRefs.current[nextIndex]?.focus();
    const nextStep = steps[nextIndex];
    if (nextStep) onSelect?.(nextStep.step_run_id);
  };

  return (
    <nav
      data-slot="step-rail"
      aria-label={ariaLabel}
      className={cn('flex flex-col gap-1 w-full', className)}
    >
      <div className="flex items-center justify-between text-2xs uppercase tracking-wider font-semibold text-fg-muted px-2 pb-2 border-b border-border mb-1">
        <span>Steps</span>
        <span className="font-mono">{steps.length}</span>
      </div>

      <div
        ref={listRef}
        data-slot="step-rail-list"
        className="flex flex-col gap-0.5 min-h-0 overflow-y-auto"
      >
        {steps.map((step, idx) => {
          const isSelected = selectedStepRunId === step.step_run_id;
          const dotColor = statusDotColors[step.status] ?? 'bg-status-queued-fg';

          return (
            <button
              key={step.step_run_id}
              ref={(el) => {
                itemRefs.current[idx] = el;
              }}
              type="button"
              aria-current={isSelected ? 'step' : undefined}
              tabIndex={step.step_run_id === tabStop ? 0 : -1}
              onClick={() => activate(step.step_run_id)}
              onKeyDown={(e) => handleKeyDown(e, idx)}
              className={cn(
                'group flex items-start gap-2.5 py-2 px-2.5 rounded-md text-left transition-colors select-none text-xs w-full focus-visible:outline-2 focus-visible:outline-accent',
                isSelected
                  ? 'bg-surface-raised border border-border shadow-xs text-fg font-medium'
                  : 'text-fg-muted hover:text-fg hover:bg-surface-raised/60',
              )}
            >
              {/* Status dot */}
              <span
                className={cn('mt-1.5 size-1.5 rounded-full shrink-0', dotColor)}
                aria-hidden="true"
              />

              <div className="flex flex-col flex-1 min-w-0">
                <div className="flex items-center justify-between gap-1.5">
                  <span
                    className={cn(
                      'truncate',
                      isSelected ? 'text-fg font-semibold' : 'group-hover:text-fg',
                    )}
                  >
                    {step.title}
                  </span>
                  {step.verdict && (
                    <span
                      className={cn(
                        'font-mono text-2xs uppercase shrink-0',
                        step.verdict === 'pass' && 'text-verdict-pass-fg',
                        step.verdict === 'warn' && 'text-verdict-warn-fg',
                        step.verdict === 'fail' && 'text-verdict-fail-fg',
                      )}
                    >
                      {step.verdict}
                    </span>
                  )}
                </div>

                <div className="flex items-center gap-2 text-2xs text-fg-muted mt-0.5 font-mono">
                  <span>{step.status}</span>
                  {step.path && (
                    <>
                      <span>·</span>
                      <span className="truncate">{step.path}</span>
                    </>
                  )}
                </div>
                {step.followup ? (
                  <div className="mt-1 flex flex-wrap gap-1">
                    <FollowupTags
                      followup={step.followup}
                      nameOf={nameOf}
                      teamNameOf={teamNameOf}
                    />
                  </div>
                ) : null}
              </div>
            </button>
          );
        })}
      </div>
    </nav>
  );
}
