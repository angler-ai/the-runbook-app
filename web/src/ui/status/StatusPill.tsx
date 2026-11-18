import type { ComponentType } from 'react';
import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import {
  AlertTriangle,
  CheckCircle2,
  CircleDot,
  CircleSlash,
  Clock,
  Hourglass,
  PauseCircle,
  Play,
  SkipForward,
  User,
  XCircle,
} from 'lucide-react';

export type StepStatus = components['schemas']['StepStatus'];
export type ExecutionStatus = components['schemas']['ExecutionStatus'];
export type AnyStatus = StepStatus | ExecutionStatus;

interface StatusConfig {
  label: string;
  icon: ComponentType<{ className?: string; 'aria-hidden'?: boolean | 'true' | 'false' }>;
  className: string;
}

const statusConfigs: Record<AnyStatus, StatusConfig> = {
  // Execution statuses
  queued: {
    label: 'Queued',
    icon: Clock,
    className: 'bg-status-queued-bg text-status-queued-fg',
  },
  running: {
    label: 'Running',
    icon: Play,
    className: 'bg-status-running-bg text-status-running-fg',
  },
  waiting: {
    label: 'Waiting',
    icon: Hourglass,
    className: 'bg-status-waiting-bg text-status-waiting-fg',
  },
  paused: {
    label: 'Paused',
    icon: PauseCircle,
    className: 'bg-status-paused-bg text-status-paused-fg',
  },
  succeeded: {
    label: 'Succeeded',
    icon: CheckCircle2,
    className: 'bg-status-succeeded-bg text-status-succeeded-fg',
  },
  failed: {
    label: 'Failed',
    icon: XCircle,
    className: 'bg-status-failed-bg text-status-failed-fg',
  },
  cancelled: {
    label: 'Cancelled',
    icon: CircleSlash,
    className: 'bg-status-cancelled-bg text-status-cancelled-fg',
  },

  // Step statuses
  pending: {
    label: 'Pending',
    icon: Clock,
    className: 'bg-status-queued-bg text-status-queued-fg',
  },
  ready: {
    label: 'Ready',
    icon: CircleDot,
    className: 'bg-status-queued-bg text-status-queued-fg',
  },
  claimed: {
    label: 'Claimed',
    icon: User,
    className: 'bg-status-running-bg text-status-running-fg',
  },
  skipped: {
    label: 'Skipped',
    icon: SkipForward,
    className: 'bg-status-skipped-bg text-status-skipped-fg',
  },
  timed_out: {
    label: 'Timed out',
    icon: AlertTriangle,
    className: 'bg-status-timed-out-bg text-status-timed-out-fg',
  },
  expired: {
    label: 'Expired',
    icon: AlertTriangle,
    className: 'bg-status-expired-bg text-status-expired-fg',
  },
};

export interface StatusPillProps {
  status: AnyStatus;
  label?: string;
  className?: string;
}

export function StatusPill({ status, label, className }: StatusPillProps) {
  const config = statusConfigs[status] ?? {
    label: status,
    icon: Clock,
    className: 'bg-status-queued-bg text-status-queued-fg',
  };

  const Icon = config.icon;
  const displayLabel = label ?? config.label;

  return (
    <span
      data-slot="status-pill"
      data-status={status}
      className={cn(
        'inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-xs font-medium select-none whitespace-nowrap',
        config.className,
        className,
      )}
    >
      <Icon className="size-3.5 shrink-0" aria-hidden="true" />
      <span>{displayLabel}</span>
    </span>
  );
}
