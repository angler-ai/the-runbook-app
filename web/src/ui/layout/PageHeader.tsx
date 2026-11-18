// UI-1: Shelf's page heading: an optional mono eyebrow, a 21px title with an
// optional count chip, a muted description, actions on the right.
import type { ReactNode } from 'react';
import { cn } from '../primitives/cn';

export interface PageHeaderProps {
  title: ReactNode;
  description?: ReactNode;
  breadcrumbs?: ReactNode;
  actions?: ReactNode;
  /** Small mono capitals above the title (for example the project). */
  eyebrow?: ReactNode;
  /** A count shown beside the title in a mono chip. */
  count?: ReactNode;
  className?: string;
}

export function PageHeader({
  title,
  description,
  breadcrumbs,
  actions,
  eyebrow,
  count,
  className,
}: PageHeaderProps) {
  return (
    <div data-slot="page-header" className={cn('mb-6 flex flex-col gap-2', className)}>
      {breadcrumbs && (
        <div data-slot="page-header-breadcrumbs" className="mb-1 text-xs text-fg-muted">
          {breadcrumbs}
        </div>
      )}

      <div className="flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
        <div className="min-w-0 flex-1">
          {eyebrow ? (
            <p className="mb-2 font-mono text-[10px] font-medium tracking-[0.12em] text-fg-muted uppercase">
              {eyebrow}
            </p>
          ) : null}
          <div className="flex min-w-0 items-center gap-3">
            <h1 className="truncate text-lg font-semibold tracking-[-0.02em] text-fg">{title}</h1>
            {count !== undefined && count !== null ? (
              <span className="shrink-0 rounded border border-border px-1.5 py-0.5 font-mono text-2xs leading-none text-fg-muted">
                {count}
              </span>
            ) : null}
          </div>
          {description && <p className="mt-1.5 max-w-2xl text-sm text-fg-muted">{description}</p>}
        </div>

        {actions && (
          <div
            data-slot="page-header-actions"
            className="flex shrink-0 flex-wrap items-center gap-2"
          >
            {actions}
          </div>
        )}
      </div>
    </div>
  );
}
