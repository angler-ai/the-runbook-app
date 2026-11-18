// UI-1: Shelf's breadcrumbs: 12px muted segments, faint chevrons, the
// current page in the text colour. Links render through `renderLink` so the
// router's Link can be used.
import { ChevronRight } from 'lucide-react';
import { Fragment, type ReactNode } from 'react';
import { cn } from '../primitives/cn';

export interface BreadcrumbItem {
  label: string;
  href?: string;
}

export interface BreadcrumbsProps {
  items: BreadcrumbItem[];
  renderLink?: (props: { href: string; className: string; children: ReactNode }) => ReactNode;
  className?: string;
}

const linkClass = 'rounded-sm hover:text-fg';

export function Breadcrumbs({ items, renderLink, className }: BreadcrumbsProps) {
  return (
    <nav aria-label="Breadcrumb" className={cn('min-w-0', className)}>
      <ol className="flex min-w-0 items-center gap-2 text-xs text-fg-muted">
        {items.map((item, index) => {
          const last = index === items.length - 1;
          return (
            <Fragment key={`${index}-${item.label}`}>
              {index > 0 ? (
                <li aria-hidden="true" className="hidden shrink-0 items-center sm:flex">
                  <ChevronRight className="size-3.5 text-border-input" />
                </li>
              ) : null}
              <li
                className={cn('min-w-0 truncate', last ? 'font-medium text-fg' : 'hidden sm:block')}
              >
                {last || !item.href ? (
                  <span aria-current={last ? 'page' : undefined}>{item.label}</span>
                ) : renderLink ? (
                  renderLink({ href: item.href, className: linkClass, children: item.label })
                ) : (
                  <a href={item.href} className={linkClass}>
                    {item.label}
                  </a>
                )}
              </li>
            </Fragment>
          );
        })}
      </ol>
    </nav>
  );
}
