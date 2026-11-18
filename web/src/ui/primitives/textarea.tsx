import type { ComponentProps } from 'react';
import { fieldClasses } from './input';
import { cn } from './cn';

export function Textarea({ className, ...props }: ComponentProps<'textarea'>) {
  return (
    <textarea
      data-slot="textarea"
      className={cn(fieldClasses, 'min-h-20 py-2', className)}
      {...props}
    />
  );
}
