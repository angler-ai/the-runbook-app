import * as SheetPrimitive from '@radix-ui/react-dialog';
import { cva, type VariantProps } from 'class-variance-authority';
import { X } from 'lucide-react';
import type { ComponentProps } from 'react';
import { cn } from './cn';
import { overlayClasses } from './dialog';

/** A dialog that slides in from an edge: the mobile navigation drawer and side panels. */
export const Sheet = SheetPrimitive.Root;
export const SheetTrigger = SheetPrimitive.Trigger;
export const SheetClose = SheetPrimitive.Close;
export const SheetPortal = SheetPrimitive.Portal;

const sheetVariants = cva(
  'fixed z-50 flex flex-col gap-4 overflow-y-auto border-border bg-surface p-6 text-fg shadow-lg focus-visible:outline-none',
  {
    variants: {
      side: {
        left: 'inset-y-0 left-0 h-full w-[85%] max-w-sm border-r',
        right: 'inset-y-0 right-0 h-full w-[85%] max-w-sm border-l',
        top: 'inset-x-0 top-0 border-b',
        bottom: 'inset-x-0 bottom-0 max-h-[85dvh] border-t',
      },
    },
    defaultVariants: { side: 'right' },
  },
);

export function SheetContent({
  className,
  children,
  side,
  ...props
}: ComponentProps<typeof SheetPrimitive.Content> & VariantProps<typeof sheetVariants>) {
  return (
    <SheetPortal>
      <SheetPrimitive.Overlay data-slot="sheet-overlay" className={overlayClasses} />
      <SheetPrimitive.Content
        data-slot="sheet-content"
        className={cn(sheetVariants({ side }), className)}
        {...props}
      >
        {children}
        <SheetPrimitive.Close className="absolute top-3 right-3 inline-flex size-11 items-center justify-center rounded-md text-fg-muted hover:text-fg focus-visible:outline-2 focus-visible:outline-accent sm:size-8">
          <X className="size-4" aria-hidden="true" />
          <span className="sr-only">Close</span>
        </SheetPrimitive.Close>
      </SheetPrimitive.Content>
    </SheetPortal>
  );
}

export function SheetHeader({ className, ...props }: ComponentProps<'div'>) {
  return <div data-slot="sheet-header" className={cn('grid gap-1.5', className)} {...props} />;
}

export function SheetTitle({ className, ...props }: ComponentProps<typeof SheetPrimitive.Title>) {
  return (
    <SheetPrimitive.Title
      data-slot="sheet-title"
      className={cn('text-md font-semibold', className)}
      {...props}
    />
  );
}

export function SheetDescription({
  className,
  ...props
}: ComponentProps<typeof SheetPrimitive.Description>) {
  return (
    <SheetPrimitive.Description
      data-slot="sheet-description"
      className={cn('text-base text-fg-muted', className)}
      {...props}
    />
  );
}
