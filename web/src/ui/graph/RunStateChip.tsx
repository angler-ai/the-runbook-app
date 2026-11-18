// UI-3: a step run's state (or a check's verdict) as a small chip on a
// graph card and in the run page. Status tokens only, so it follows the
// theme; the dot pulses only while running, and only without reduced motion.
import { cn } from '../primitives/cn';
import type { RunState } from './overlay';

export const RUN_STATE_STYLE: Record<RunState, { chip: string; dot: string }> = {
  pending: { chip: 'bg-status-queued-bg text-status-queued-fg', dot: 'bg-status-queued-fg/60' },
  ready: { chip: 'bg-status-queued-bg text-status-queued-fg', dot: 'bg-status-queued-fg' },
  running: {
    chip: 'bg-status-running-bg text-status-running-fg',
    dot: 'bg-status-running-fg motion-safe:animate-pulse',
  },
  waiting: { chip: 'bg-status-waiting-bg text-status-waiting-fg', dot: 'bg-status-waiting-fg' },
  succeeded: {
    chip: 'bg-status-succeeded-bg text-status-succeeded-fg',
    dot: 'bg-status-succeeded-fg',
  },
  failed: { chip: 'bg-status-failed-bg text-status-failed-fg', dot: 'bg-status-failed-fg' },
  skipped: { chip: 'bg-status-skipped-bg text-status-skipped-fg', dot: 'bg-status-skipped-fg' },
  cancelled: {
    chip: 'bg-status-cancelled-bg text-status-cancelled-fg',
    dot: 'bg-status-cancelled-fg',
  },
  timed_out: {
    chip: 'bg-status-timed-out-bg text-status-timed-out-fg',
    dot: 'bg-status-timed-out-fg',
  },
  expired: { chip: 'bg-status-expired-bg text-status-expired-fg', dot: 'bg-status-expired-fg' },
};

const VERDICT_STYLE = {
  pass: 'border-verdict-pass-border bg-verdict-pass-bg text-verdict-pass-fg',
  warn: 'border-verdict-warn-border bg-verdict-warn-bg text-verdict-warn-fg',
  fail: 'border-verdict-fail-border bg-verdict-fail-bg text-verdict-fail-fg',
} as const;

const VERDICT_WORDS = { pass: 'Pass', warn: 'Warn', fail: 'Fail' } as const;

export function RunStateChip({
  state,
  label,
  verdict,
  className,
}: {
  state: RunState;
  label: string;
  verdict?: 'pass' | 'warn' | 'fail' | undefined;
  className?: string;
}) {
  if (verdict) {
    return (
      <span
        data-slot="run-verdict"
        data-verdict={verdict}
        className={cn(
          'inline-flex items-center gap-1 rounded-sm border px-1.5 py-px text-2xs font-semibold',
          VERDICT_STYLE[verdict],
          className,
        )}
      >
        {VERDICT_WORDS[verdict]}
      </span>
    );
  }
  const style = RUN_STATE_STYLE[state];
  return (
    <span
      data-slot="run-state"
      data-state={state}
      className={cn(
        'inline-flex items-center gap-1 rounded-full px-1.5 py-px text-2xs font-medium whitespace-nowrap',
        style.chip,
        className,
      )}
    >
      <span aria-hidden="true" className={cn('size-1.5 shrink-0 rounded-full', style.dot)} />
      {label}
    </span>
  );
}
