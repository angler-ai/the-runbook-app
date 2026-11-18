import { Slot } from '@radix-ui/react-slot';
import { cva, type VariantProps } from 'class-variance-authority';
import type { ComponentProps } from 'react';
import { cn } from './cn';

export const buttonVariants = cva(
  'inline-flex shrink-0 items-center justify-center gap-2 rounded-md text-sm font-medium whitespace-nowrap transition-colors duration-[120ms] select-none active:translate-y-px focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0',
  {
    variants: {
      variant: {
        // Angler's signature hover is lighter (Light Indigo), with Ink text on it.
        primary: 'bg-accent text-accent-fg hover:bg-accent-hover hover:text-accent-hover-fg',
        secondary: 'border border-border-strong bg-surface text-fg hover:bg-hover',
        ghost: 'text-fg-muted hover:bg-hover hover:text-fg',
        // Destructive actions always go through a confirmation dialog (DESIGN.md §4).
        destructive: 'bg-danger text-danger-fg hover:opacity-90',
        link: 'text-accent underline-offset-4 hover:text-accent-strong hover:underline',
      },
      // 44px touch targets on small screens, 36px from `sm` up (DESIGN.md §4–5).
      size: {
        default: 'h-11 px-4 sm:h-9',
        sm: 'h-11 px-3 text-xs sm:h-8',
        icon: 'size-11 sm:size-9',
      },
    },
    defaultVariants: { variant: 'primary', size: 'default' },
  },
);

export type ButtonProps = ComponentProps<'button'> &
  VariantProps<typeof buttonVariants> & {
    /** Render the child element (for example a link) with button styling. */
    asChild?: boolean;
  };

export function Button({ className, variant, size, asChild = false, type, ...props }: ButtonProps) {
  const Comp = asChild ? Slot : 'button';
  return (
    <Comp
      data-slot="button"
      // Plain buttons never submit a form by accident.
      type={asChild ? type : (type ?? 'button')}
      className={cn(buttonVariants({ variant, size }), className)}
      {...props}
    />
  );
}
