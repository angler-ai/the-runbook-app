// The left side of a list-and-detail page (Webhooks, a runbook's Runs,
// Skills, Memory): a heading with a count, then one row per item. A row is
// a button: clicking anywhere on it selects it; the selected one is ringed
// in the accent. One look everywhere (2026-11-17).
import type { ReactNode } from 'react';
import { cn } from '../primitives/cn';

export function ListPaneHeading({
  id,
  title,
  count,
  more = false,
}: {
  id: string;
  title: string;
  count: number;
  /** More exist than are loaded: the count reads "25+". */
  more?: boolean;
}) {
  return (
    <h2 id={id} className="mb-3 px-1 text-sm font-semibold text-fg">
      {title} ({count}
      {more ? '+' : ''})
    </h2>
  );
}

export function ListPaneItems({ label, children }: { label?: string; children: ReactNode }) {
  return (
    <ul aria-label={label} className="grid gap-1.5">
      {children}
    </ul>
  );
}

export function ListPaneItem({
  selected,
  onSelect,
  icon,
  title,
  trailing,
  detail,
  slot,
}: {
  selected: boolean;
  onSelect(): void;
  /** A small leading icon or picture. */
  icon?: ReactNode;
  title: ReactNode;
  /** A status or count on the right of the title. */
  trailing?: ReactNode;
  /** One muted line under the title. */
  detail?: ReactNode;
  /** `data-slot` for the row's button. */
  slot?: string;
}) {
  return (
    <li>
      <button
        type="button"
        data-slot={slot}
        aria-current={selected ? 'true' : undefined}
        onClick={onSelect}
        className={cn(
          'grid w-full gap-1 rounded-lg border px-3 py-2.5 text-left transition-colors focus-visible:outline-2 focus-visible:outline-accent',
          selected
            ? 'border-accent bg-accent/5 ring-1 ring-accent'
            : 'border-border hover:bg-surface-raised',
        )}
      >
        <span className="flex items-center justify-between gap-2">
          <span className="flex min-w-0 items-center gap-2">
            {icon ? <span className="flex shrink-0 text-fg-muted">{icon}</span> : null}
            <span className="min-w-0 truncate text-sm font-medium text-fg">{title}</span>
          </span>
          {trailing ? <span className="flex shrink-0 items-center gap-1.5">{trailing}</span> : null}
        </span>
        {detail ? <span className="truncate text-xs text-fg-muted">{detail}</span> : null}
      </button>
    </li>
  );
}
