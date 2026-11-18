// UI-1: Shelf's quiet nav rows: 36px, 13px muted text; hover lifts to the
// text colour on a slightly darker tint; the current page sits on the accent's
// subtle tint in the accent. Counts are small mono figures.
import type { ComponentType, ReactNode } from 'react';
import { cn } from '../primitives/cn';

export interface SidebarNavItem {
  id: string;
  label: string;
  href: string;
  icon?: ComponentType<{ className?: string; 'aria-hidden'?: boolean | 'true' | 'false' }>;
  /** A count badge; a string such as "1000+" when the count is a lower bound. */
  count?: number | string;
  active?: boolean;
  /** Not available yet: rendered as text with `note` (for example "Soon"), never as a link. */
  disabled?: boolean;
  note?: string;
}

export interface RenderLinkProps {
  item: SidebarNavItem;
  className: string;
  children: ReactNode;
}

export interface SidebarNavProps {
  items: SidebarNavItem[];
  renderLink?: (props: RenderLinkProps) => ReactNode;
  className?: string;
  ariaLabel?: string;
  /** Icons only, for the collapsed sidebar's narrow rail: each label is a tooltip and the link's name. */
  compact?: boolean;
}

const compactClasses =
  'relative flex size-9 items-center justify-center rounded-[5px] transition-colors duration-[120ms] select-none';

export const navRowClasses =
  'group flex h-9 w-full items-center gap-2.5 rounded-[5px] px-2.5 text-sm transition-colors duration-[120ms] select-none';

export function SidebarNav({
  items,
  renderLink,
  className,
  ariaLabel = 'Sidebar navigation',
  compact = false,
}: SidebarNavProps) {
  return (
    <nav
      aria-label={ariaLabel}
      className={cn('flex flex-col gap-0.5', compact && 'items-center gap-1', className)}
    >
      {items.map((item) => {
        const Icon = item.icon;
        const compactContent = (
          <>
            {Icon && <Icon className="size-[18px] shrink-0" aria-hidden="true" />}
            <span className="sr-only">
              {item.label}
              {item.count !== undefined && item.count !== 0 ? `, ${item.count}` : ''}
            </span>
            {item.count !== undefined && item.count !== 0 ? (
              <span
                data-slot="sidebar-nav-count"
                aria-hidden="true"
                className="absolute -top-0.5 -right-0.5 min-w-4 rounded-full bg-accent px-1 text-center font-mono text-[10px] leading-4 text-accent-fg tabular-nums"
              >
                {item.count}
              </span>
            ) : null}
          </>
        );
        const content = compact ? (
          compactContent
        ) : (
          <>
            {Icon && <Icon className="size-4 shrink-0" aria-hidden="true" />}
            <span className="min-w-0 flex-1 truncate">{item.label}</span>
            {item.count !== undefined && (
              <span
                data-slot="sidebar-nav-count"
                className={cn(
                  'font-mono text-2xs tabular-nums',
                  item.active ? 'text-accent' : 'text-fg-muted',
                )}
              >
                {item.count}
              </span>
            )}
            {item.note ? (
              <span className="rounded border border-border-strong px-1 font-mono text-[10px] leading-4 tracking-wide text-fg-muted uppercase">
                {item.note}
              </span>
            ) : null}
          </>
        );

        if (item.disabled) {
          return (
            <span
              key={item.id}
              aria-disabled="true"
              title={compact ? item.label : undefined}
              className={cn(
                compact ? compactClasses : navRowClasses,
                'cursor-default text-fg-muted',
              )}
            >
              {content}
            </span>
          );
        }

        const linkClass = cn(
          compact ? compactClasses : navRowClasses,
          item.active
            ? 'bg-accent-subtle font-medium text-accent'
            : 'text-fg-muted hover:bg-sidebar-hover hover:text-fg',
        );

        if (renderLink) {
          return (
            <div key={item.id} title={compact ? item.label : undefined}>
              {renderLink({ item, className: linkClass, children: content })}
            </div>
          );
        }

        return (
          <a
            key={item.id}
            href={item.href}
            aria-current={item.active ? 'page' : undefined}
            title={compact ? item.label : undefined}
            className={linkClass}
          >
            {content}
          </a>
        );
      })}
    </nav>
  );
}

/** Shelf's section label: small mono capitals, letter-spaced. */
export function SidebarSectionLabel({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <p
      className={cn(
        'px-2.5 pb-2 font-mono text-[10px] font-medium tracking-[0.12em] text-fg-muted uppercase',
        className,
      )}
    >
      {children}
    </p>
  );
}
