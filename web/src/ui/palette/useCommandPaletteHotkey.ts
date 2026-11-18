import { useEffect } from 'react';

/**
 * Checks whether the active focused element is a text input, textarea, or contentEditable element.
 */
export function isInputElement(target: EventTarget | null): boolean {
  if (!target || !(target instanceof HTMLElement)) {
    return false;
  }
  const tag = target.tagName;
  if (
    tag === 'INPUT' ||
    tag === 'TEXTAREA' ||
    target.isContentEditable ||
    target.contentEditable === 'true' ||
    target.getAttribute('contenteditable') === 'true' ||
    target.getAttribute('contenteditable') === ''
  ) {
    return true;
  }
  return false;
}

/**
 * Hook for ⌘K / Ctrl+K keyboard shortcut to open/close the command palette.
 *
 * Rules:
 * - Must NOT fire while typing in an input or textarea, UNLESS the palette is already open.
 * - Supports both ⌘K on macOS and Ctrl+K on Windows/Linux.
 */
export function useCommandPaletteHotkey(
  open: boolean,
  setOpen: (open: boolean | ((prev: boolean) => boolean)) => void,
) {
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const isK = e.key === 'k' || e.key === 'K';
      const isModifier = e.metaKey || e.ctrlKey;

      if (!isK || !isModifier) {
        return;
      }

      // If palette is closed and user is typing in an input/textarea, do not intercept
      if (!open && isInputElement(e.target)) {
        return;
      }

      e.preventDefault();
      setOpen((prev) => !prev);
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [open, setOpen]);
}
