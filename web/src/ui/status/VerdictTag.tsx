import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';

export type Verdict = components['schemas']['Verdict'];

export interface VerdictTagProps {
  verdict?: Verdict | null;
  className?: string;
}

const verdictStyles: Record<Verdict, { label: string; className: string }> = {
  pass: {
    label: 'Verdict pass',
    className: 'border-verdict-pass-border text-verdict-pass-fg bg-verdict-pass-bg',
  },
  warn: {
    label: 'Verdict warn',
    className: 'border-verdict-warn-border text-verdict-warn-fg bg-verdict-warn-bg',
  },
  fail: {
    label: 'Verdict fail',
    className: 'border-verdict-fail-border text-verdict-fail-fg bg-verdict-fail-bg',
  },
};

const noVerdictStyle = {
  label: 'No verdict',
  className: 'border-verdict-none-border text-verdict-none-fg bg-verdict-none-bg',
};

export function VerdictTag({ verdict, className }: VerdictTagProps) {
  const current = verdict ? verdictStyles[verdict] ?? noVerdictStyle : noVerdictStyle;

  return (
    <span
      data-slot="verdict-tag"
      data-verdict={verdict ?? 'none'}
      className={cn(
        'inline-flex items-center w-fit self-start shrink-0 rounded-sm border px-1.5 py-0.5 text-2xs font-medium select-none whitespace-nowrap',
        current.className,
        className,
      )}
    >
      {current.label}
    </span>
  );
}
