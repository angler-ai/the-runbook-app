import type { HTMLAttributes, ReactNode } from 'react';
import { cn } from '../primitives/cn';

export interface MonoRefProps extends HTMLAttributes<HTMLElement> {
  children: ReactNode;
  as?: 'span' | 'code';
  className?: string;
}

export function MonoRef({ children, as = 'code', className, ...props }: MonoRefProps) {
  const Comp = as;

  return (
    <Comp
      data-slot="mono-ref"
      className={cn(
        'font-mono text-xs text-fg selection:bg-accent/20 tracking-tight',
        className,
      )}
      {...props}
    >
      {children}
    </Comp>
  );
}
