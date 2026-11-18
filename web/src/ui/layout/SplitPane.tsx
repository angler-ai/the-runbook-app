import { useRef, type CSSProperties, type ReactNode } from 'react';
import { useFillHeight } from '../graph/fillHeight';
import { cn } from '../primitives/cn';

export interface SplitPaneProps {
  /** Content for the left (or primary) pane */
  left: ReactNode;
  /** Content for the right (or detail) pane */
  right: ReactNode;
  /** Width class for the left pane on desktop (defaults to 'md:w-[55%]') */
  leftWidth?: string;
  className?: string;
  leftClassName?: string;
  rightClassName?: string;
  ariaLabel?: string;
  /**
   * From md up, reach down to the bottom of the screen, each pane scrolling
   * on its own (a long list does not push the detail out of view).
   */
  fillViewport?: boolean;
}

export function SplitPane({
  left,
  right,
  leftWidth = 'md:w-[55%]',
  className,
  leftClassName,
  rightClassName,
  ariaLabel = 'Split pane layout',
  fillViewport = false,
}: SplitPaneProps) {
  const ref = useRef<HTMLDivElement>(null);
  const height = useFillHeight(ref, fillViewport);
  const style =
    height != null ? ({ '--split-pane-height': `${height}px` } as CSSProperties) : undefined;
  return (
    <div
      ref={ref}
      data-slot="split-pane"
      aria-label={ariaLabel}
      style={style}
      className={cn(
        'flex flex-col md:flex-row min-h-0 flex-1 w-full border border-border rounded-lg bg-surface overflow-hidden',
        height != null && 'md:h-[var(--split-pane-height)]',
        className,
      )}
    >
      {/* Left / primary pane */}
      <div
        data-slot="split-pane-left"
        className={cn(
          'w-full border-b md:border-b-0 md:border-r border-border overflow-y-auto p-4 sm:p-6',
          leftWidth,
          leftClassName,
        )}
      >
        {left}
      </div>

      {/* Right / detail pane */}
      <div
        data-slot="split-pane-right"
        className={cn('flex-1 min-w-0 overflow-y-auto p-4 sm:p-6', rightClassName)}
      >
        {right}
      </div>
    </div>
  );
}
