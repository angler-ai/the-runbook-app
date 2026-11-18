import * as TabsPrimitive from '@radix-ui/react-tabs';
import type { ComponentProps } from 'react';
import { cn } from './cn';

export const Tabs = TabsPrimitive.Root;

/** Underlined tabs on a hairline (DESIGN.md §0: no boxed chrome). */
export function TabsList({ className, ...props }: ComponentProps<typeof TabsPrimitive.List>) {
  return (
    <TabsPrimitive.List
      data-slot="tabs-list"
      className={cn(
        // Scrolls sideways when the tabs don't fit (phones); the hairline is an
        // inset shadow so the active tab's underline still sits on it.
        'flex items-center gap-4 overflow-x-auto overflow-y-hidden shadow-[inset_0_-1px_0_var(--color-border)] [scrollbar-width:none] [&::-webkit-scrollbar]:hidden',
        className,
      )}
      {...props}
    />
  );
}

export function TabsTrigger({ className, ...props }: ComponentProps<typeof TabsPrimitive.Trigger>) {
  return (
    <TabsPrimitive.Trigger
      data-slot="tabs-trigger"
      className={cn(
        'inline-flex h-11 shrink-0 items-center border-b-2 whitespace-nowrap border-transparent px-1 text-base font-medium text-fg-muted hover:text-fg focus-visible:outline-2 focus-visible:outline-accent disabled:pointer-events-none disabled:opacity-50 data-[state=active]:border-accent data-[state=active]:text-fg sm:h-9',
        className,
      )}
      {...props}
    />
  );
}

export function TabsContent({ className, ...props }: ComponentProps<typeof TabsPrimitive.Content>) {
  return (
    <TabsPrimitive.Content
      data-slot="tabs-content"
      className={cn('pt-4 focus-visible:outline-2 focus-visible:outline-accent', className)}
      {...props}
    />
  );
}
