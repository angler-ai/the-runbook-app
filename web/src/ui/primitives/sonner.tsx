import type { CSSProperties } from 'react';
import { Toaster as Sonner, type ToasterProps } from 'sonner';

/**
 * Toast host. Mount once (App does this); raise toasts with `toast()` from 'sonner'.
 * `theme` defaults to the system preference; pass the resolved theme from the theme
 * provider (A1) once it exists.
 */
export function Toaster({ theme = 'system', ...props }: ToasterProps) {
  return (
    <Sonner
      theme={theme}
      style={
        {
          '--normal-bg': 'var(--color-surface-raised)',
          '--normal-text': 'var(--color-fg)',
          '--normal-border': 'var(--color-border)',
        } as CSSProperties
      }
      {...props}
    />
  );
}

export { toast } from 'sonner';
