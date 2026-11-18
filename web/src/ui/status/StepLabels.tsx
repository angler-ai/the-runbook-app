import { cn } from '../primitives/cn';

export interface StepLabelsProps {
  /** LANG9: a step's labels from its definition (`audience: customer`). */
  labels?: Record<string, string> | null;
  className?: string;
}

/** A step's labels as small `key: value` chips; nothing when it has none. */
export function StepLabels({ labels, className }: StepLabelsProps) {
  const entries = Object.entries(labels ?? {});
  if (entries.length === 0) return null;
  return (
    <ul
      data-slot="step-labels"
      aria-label="Labels"
      className={cn('flex flex-wrap items-center gap-1', className)}
    >
      {entries.map(([key, value]) => (
        <li
          key={key}
          className="inline-flex items-center rounded-sm border border-border bg-surface-raised px-1.5 py-0.5 text-2xs text-fg-muted whitespace-nowrap"
        >
          <span className="font-medium text-fg">{key}</span>
          <span aria-hidden="true">:</span>
          <span className="ml-0.5">{value}</span>
        </li>
      ))}
    </ul>
  );
}
