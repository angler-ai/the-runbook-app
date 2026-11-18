import { useState } from 'react';
import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { MonoRef } from '../status/MonoRef';
import { AlertTriangle, ChevronRight, X } from 'lucide-react';

export type UndeliveredNotification = components['schemas']['UndeliveredNotification'];

export interface UndeliveredNoticeProps {
  notifications?: UndeliveredNotification[] | null;
  className?: string;
  /**
   * UI-3: one quiet line that opens to the list and can be dismissed (it
   * comes back when another notification goes undelivered). A review stack
   * with no webhook subscriber has some on every run, so the full box
   * would sit above every graph. `dismissKey` remembers the dismissal for
   * the tab session (the run's ID).
   */
  compact?: boolean;
  dismissKey?: string;
}

function readDismissed(key: string | undefined): number {
  if (!key) return 0;
  try {
    return Number(sessionStorage.getItem(`rb.undelivered.${key}`) ?? 0) || 0;
  } catch {
    return 0;
  }
}

function NotificationList({ notifications }: { notifications: UndeliveredNotification[] }) {
  return (
    <ul className="mt-2 flex flex-col gap-1.5 border-t border-verdict-warn-border/30 pt-2">
      {notifications.map((item, idx) => (
        <li key={idx} className="flex flex-wrap items-center gap-2 font-mono text-2xs">
          <span className="font-medium text-fg">{item.event_type}</span>
          <span className="text-fg-muted">({item.reason})</span>
          <MonoRef>{item.event_id}</MonoRef>
          {item.step_run_id && (
            <>
              <span className="text-fg-muted">at</span>
              <MonoRef>{item.step_run_id}</MonoRef>
            </>
          )}
        </li>
      ))}
    </ul>
  );
}

function CompactNotice({
  notifications,
  className,
  dismissKey,
}: {
  notifications: UndeliveredNotification[];
  className?: string | undefined;
  dismissKey?: string | undefined;
}) {
  const [dismissed, setDismissed] = useState(() => readDismissed(dismissKey));
  const count = notifications.length;
  if (count <= dismissed) return null;
  const dismiss = () => {
    setDismissed(count);
    if (!dismissKey) return;
    try {
      sessionStorage.setItem(`rb.undelivered.${dismissKey}`, String(count));
    } catch {
      // Storage off: the dismissal lasts until the page reloads.
    }
  };
  return (
    <div className={cn('flex items-start gap-1 text-xs print:hidden', className)}>
      <details data-slot="undelivered-notice" className="group min-w-0 flex-1">
        <summary className="inline-flex cursor-pointer select-none items-center gap-1.5 text-fg-muted hover:text-fg">
          <ChevronRight
            className="size-3.5 shrink-0 transition-transform group-open:rotate-90"
            aria-hidden="true"
          />
          <AlertTriangle className="size-3.5 shrink-0 text-verdict-warn-fg" aria-hidden="true" />
          <span>
            {count} {count === 1 ? 'notification' : 'notifications'} not delivered
          </span>
        </summary>
        <div className="mt-1 rounded-md border border-border bg-surface-raised px-3 py-2">
          <p className="text-2xs text-fg-muted">
            They reached no subscriber or failed delivery. The run is not affected.
          </p>
          <NotificationList notifications={notifications} />
        </div>
      </details>
      <button
        type="button"
        onClick={dismiss}
        aria-label="Dismiss the undelivered notifications notice"
        className="inline-flex size-5 shrink-0 items-center justify-center rounded-sm text-fg-muted hover:bg-surface-raised hover:text-fg"
      >
        <X className="size-3.5" aria-hidden="true" />
      </button>
    </div>
  );
}

export function UndeliveredNotice({
  notifications,
  className,
  compact = false,
  dismissKey,
}: UndeliveredNoticeProps) {
  if (!notifications || notifications.length === 0) {
    return null;
  }
  if (compact) {
    // Keyed by the run: a dismissal belongs to its run, so moving to another
    // run on the same page starts from that run's own stored dismissal.
    return (
      <CompactNotice
        key={dismissKey ?? ''}
        notifications={notifications}
        className={className}
        dismissKey={dismissKey}
      />
    );
  }

  return (
    <div
      role="status"
      data-slot="undelivered-notice"
      className={cn(
        'p-3 sm:p-4 rounded-md border border-verdict-warn-border bg-verdict-warn-bg text-verdict-warn-fg text-xs flex items-start gap-3',
        className,
      )}
    >
      <AlertTriangle className="size-4 shrink-0 mt-0.5 text-verdict-warn-fg" aria-hidden="true" />
      <div className="flex-1 min-w-0">
        <h4 className="font-semibold text-fg">
          Notifications not delivered ({notifications.length})
        </h4>
        <p className="mt-0.5 text-2xs text-fg-muted">
          The following notifications reached no subscriber or failed delivery:
        </p>

        <NotificationList notifications={notifications} />
      </div>
    </div>
  );
}
