import type { ComponentProps } from 'react';
import { cn } from './cn';

export const fieldClasses =
  'w-full min-w-0 rounded-md border border-border-input bg-surface px-3 text-base text-fg placeholder:text-fg-muted focus-visible:outline-2 focus-visible:outline-offset-0 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-danger';

export function Input({ className, type = 'text', ...props }: ComponentProps<'input'>) {
  return (
    <input
      data-slot="input"
      type={type}
      className={cn(fieldClasses, 'h-11 sm:h-9', className)}
      {...props}
    />
  );
}
