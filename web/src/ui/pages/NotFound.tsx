import type { ReactNode } from 'react';
import { EmptyState } from '../layout/EmptyState';
import { FileQuestion } from 'lucide-react';
import { cn } from '../primitives/cn';

export interface NotFoundProps {
  renderLink?: (props: { href: string; className: string; children: ReactNode }) => ReactNode;
  /** Where the way back leads: My tasks by default (sign-in when signed out). */
  target?: { href: string; label: string };
  className?: string;
}

const MY_TASKS = { href: '/tasks', label: 'Go to My tasks' };

export function NotFound({ renderLink, target = MY_TASKS, className }: NotFoundProps) {
  const targetHref = target.href;
  const label = target.label;
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
      data-slot="page-not-found"
      className={cn('flex flex-1 items-center justify-center p-6 min-h-[400px]', className)}
    >
      <EmptyState
        icon={FileQuestion}
        headingLevel={1}
        title="Page not found"
        description="The page you are looking for doesn't exist, has been moved, or you don't have access to it."
        action={action}
      />
    </div>
  );
}
