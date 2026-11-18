import type { ReactNode } from 'react';
import { EmptyState } from '../layout/EmptyState';
import { ShieldAlert } from 'lucide-react';
import { cn } from '../primitives/cn';

export interface ForbiddenProps {
  renderLink?: (props: { href: string; className: string; children: ReactNode }) => ReactNode;
  className?: string;
}

export function Forbidden({ renderLink, className }: ForbiddenProps) {
  const targetHref = '/tasks';
  const label = 'Go to My tasks';
  const linkClass =
    'inline-flex items-center justify-center px-4 py-2 rounded-md bg-accent text-accent-fg font-medium text-xs hover:bg-accent-hover transition-colors focus-visible:outline-2 focus-visible:outline-accent';

  const action = renderLink ? (
    renderLink({
      href: targetHref,
      className: linkClass,
      children: label,
    })
  ) : (
    <a href={targetHref} className={linkClass}>
      {label}
    </a>
  );

  return (
    <div
      data-slot="page-forbidden"
      className={cn('flex flex-1 items-center justify-center p-6 min-h-[400px]', className)}
    >
      <EmptyState
        icon={ShieldAlert}
        title="Access denied"
        description="You do not have permission to view this resource. Contact your organization administrator for access."
        action={action}
      />
    </div>
  );
}
