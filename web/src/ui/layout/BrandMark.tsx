// The Runbook's mark: three steps as bars, two done in the accent and the
// current one shorter, in mint. Drawn from theme colours so an operator's
// colour override recolours it. public/icons/icon.svg is the same mark on an
// indigo tile, for the browser tab and app icons.
import { cn } from '../primitives/cn';

export function BrandMark({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 48 48"
      aria-hidden="true"
      focusable="false"
      data-slot="brand-mark"
      className={cn('size-6 shrink-0 text-accent', className)}
    >
      <rect x="7" y="9" width="34" height="7" rx="3.5" fill="currentColor" />
      <rect x="7" y="20.5" width="34" height="7" rx="3.5" fill="currentColor" />
      <rect x="7" y="32" width="20" height="7" rx="3.5" fill="var(--color-mint)" />
    </svg>
  );
}

/** The product mark and name, as the sidebar shows them. */
export function BrandLockup({ className }: { className?: string }) {
  return (
    <span className={cn('flex items-center gap-2.5', className)}>
      <BrandMark />
      <span className="text-[17px] font-semibold tracking-[-0.03em] text-fg">The Runbook</span>
    </span>
  );
}
