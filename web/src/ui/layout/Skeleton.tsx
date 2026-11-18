import type { HTMLAttributes } from 'react';
import { cn } from '../primitives/cn';

export interface SkeletonProps extends HTMLAttributes<HTMLDivElement> {
  className?: string;
}

export function Skeleton({ className, ...props }: SkeletonProps) {
  return (
    <div
      data-slot="skeleton"
      aria-hidden="true"
      className={cn('animate-pulse rounded-md bg-border/60', className)}
      {...props}
    />
  );
}
