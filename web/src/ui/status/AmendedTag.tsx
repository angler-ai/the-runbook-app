import { cn } from '../primitives/cn';

export interface AmendedTagProps {
  className?: string;
  label?: string;
}

export function AmendedTag({ className, label = 'Amended' }: AmendedTagProps) {
  return (
    <span
      data-slot="amended-tag"
      className={cn(
        'inline-flex items-center rounded-sm border border-dashed border-amended-border bg-amended-bg text-amended-fg px-1.5 py-0.5 text-2xs font-medium select-none whitespace-nowrap',
        className,
      )}
    >
      {label}
    </span>
  );
}
