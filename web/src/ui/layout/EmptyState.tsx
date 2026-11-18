import type { ComponentType, ReactNode } from 'react';
import { cn } from '../primitives/cn';

export interface EmptyStateProps {
  icon?: ComponentType<{ className?: string; 'aria-hidden'?: boolean | 'true' | 'false' }>;
  title: string;
  description?: string;
  action?: ReactNode;
  /** The title's heading level: 1 when the empty state is the whole page. */
  headingLevel?: 1 | 2 | 3;
  className?: string;
}

export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
  headingLevel = 3,
  className,
}: EmptyStateProps) {
  const Heading = `h${headingLevel}` as const;
  return (
    <div
      data-slot="empty-state"
      className={cn(
        'flex flex-col items-center justify-center text-center p-8 sm:p-12 border border-dashed border-border rounded-lg bg-surface/50 max-w-lg mx-auto',
        className,
      )}
    >
      {Icon && (
        <div className="flex size-10 items-center justify-center rounded-full bg-surface-raised border border-border text-fg-muted mb-3">
          <Icon className="size-5" aria-hidden="true" />
        </div>
      )}

      <Heading className="text-sm font-semibold text-fg">{title}</Heading>

      {description && <p className="mt-1 text-xs text-fg-muted max-w-sm">{description}</p>}

      {action && <div className="mt-4">{action}</div>}
    </div>
  );
}
