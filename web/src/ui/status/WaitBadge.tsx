// The badge that says what a run or step waits on (UI-2's Home; UI-3's
// run graph shares it): an agent, a person, an approval, a signal, a
// timer, a child run, admission or a resume.
import {
  AlarmClock,
  Bot,
  Clock3,
  GitBranch,
  PauseCircle,
  Radio,
  ShieldCheck,
  UserRound,
  type LucideIcon,
} from 'lucide-react';
import { cn } from '../primitives/cn';

export type WaitKind =
  'agent' | 'person' | 'approval' | 'signal' | 'timer' | 'child' | 'queued' | 'paused';

const WAIT_STYLE: Record<WaitKind, { icon: LucideIcon; className: string; noun: string }> = {
  agent: { icon: Bot, className: 'bg-accent-subtle text-accent', noun: 'An agent' },
  person: {
    icon: UserRound,
    className: 'bg-status-waiting-bg text-status-waiting-fg',
    noun: 'A person',
  },
  approval: {
    icon: ShieldCheck,
    className: 'bg-status-paused-bg text-status-paused-fg',
    noun: 'An approval',
  },
  signal: { icon: Radio, className: 'bg-mint-subtle text-mint-text', noun: 'A signal' },
  timer: {
    icon: AlarmClock,
    className: 'bg-status-queued-bg text-status-queued-fg',
    noun: 'A timer',
  },
  child: {
    icon: GitBranch,
    className: 'bg-status-queued-bg text-status-queued-fg',
    noun: 'A child run',
  },
  queued: {
    icon: Clock3,
    className: 'bg-status-queued-bg text-status-queued-fg',
    noun: 'Admission',
  },
  paused: {
    icon: PauseCircle,
    className: 'bg-status-paused-bg text-status-paused-fg',
    noun: 'A resume',
  },
};

export function WaitBadge({
  kind,
  label,
  step,
  className,
}: {
  kind: WaitKind;
  label: string;
  step?: string;
  className?: string;
}) {
  const style = WAIT_STYLE[kind];
  const Icon = style.icon;
  return (
    <span
      data-slot="wait-badge"
      data-kind={kind}
      title={step ? `${style.noun}: ${step}` : style.noun}
      className={cn(
        'inline-flex max-w-full items-center gap-1 rounded px-1.5 py-0.5 text-2xs font-medium',
        style.className,
        className,
      )}
    >
      <Icon className="size-3 shrink-0" aria-hidden="true" />
      <span className="truncate">{label}</span>
    </span>
  );
}
