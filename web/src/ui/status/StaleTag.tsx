import { cn } from '../primitives/cn';

export interface StaleTagProps {
  className?: string;
  label?: string;
}

export function StaleTag({ className, label = 'Stale' }: StaleTagProps) {
  return (
    <span
      data-slot="stale-tag"
      className={cn(
        'inline-flex items-center rounded-sm border border-dashed border-stale-border bg-stale-bg text-stale-fg px-1.5 py-0.5 text-2xs font-medium select-none whitespace-nowrap',
        className,
      )}
    >
      {label}
    </span>
  );
}
