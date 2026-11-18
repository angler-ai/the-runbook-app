// The graph fills the screen below where it starts: on a tall screen it
// uses all the height (the side panel too), on a short one it keeps a
// usable minimum and the page scrolls. Re-measured when the window or the
// page above it changes size (a header growing, a panel opening).
import { useLayoutEffect, useState, type RefObject } from 'react';

/** Bottom breathing room: the shell's own bottom padding (lg:py-7). */
export const FILL_BOTTOM_PX = 28;
export const FILL_MIN_PX = 480;

/** The height that reaches the viewport's bottom from `top` (page px). */
export function fillHeight(viewportHeight: number, top: number, scrollY: number): number {
  return Math.max(FILL_MIN_PX, Math.round(viewportHeight - (top + scrollY) - FILL_BOTTOM_PX));
}

export function useFillHeight(ref: RefObject<HTMLElement | null>, enabled: boolean): number | null {
  const [height, setHeight] = useState<number | null>(null);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!enabled || !element) return undefined;
    const measure = () => {
      const top = element.getBoundingClientRect().top;
      const next = fillHeight(window.innerHeight, top, window.scrollY);
      setHeight((current) => (current === next ? current : next));
    };
    measure();
    window.addEventListener('resize', measure);
    // The page above can change size without a window resize.
    const observer =
      typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(() => measure());
    observer?.observe(document.body);
    return () => {
      window.removeEventListener('resize', measure);
      observer?.disconnect();
    };
  }, [ref, enabled]);
  return enabled ? height : null;
}
