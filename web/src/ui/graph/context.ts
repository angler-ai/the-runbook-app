// UI-2: what node cards read from RunbookGraph without going through the
// (pure) layout: selection, collapse and the host's call links.
import { createContext, useContext, type MouseEvent, type ReactNode } from 'react';
import type { OutlineNode } from '../types';
import type { InsertPoint, SlotData } from './layout';
import type { GraphOverlay } from './overlay';

/**
 * UI-4: what the builder adds to the graph. The graph stays a drawing; the
 * builder renders its own controls into the places the graph leaves.
 */
export interface GraphBuilder {
  /** The "+" on an edge into a step (insert before it). */
  renderInsert(point: InsertPoint, label: string): ReactNode;
  /** The pill at the end of a list. */
  renderSlot(slot: SlotData['slot']): ReactNode;
  /** A card's actions (menu, drag handle). */
  renderActions(node: OutlineNode): ReactNode;
  /** Problems per graph node ID (the card is ringed and counts them). */
  issues?: ReadonlyMap<string, number>;
}

export interface GraphContextValue {
  selectedId: string | undefined;
  select: (id: string) => void;
  toggle: (id: string) => void;
  callHref?: (slug: string) => string | undefined;
  /** UI-3: a run's state over the cards. */
  overlay?: GraphOverlay;
  /** UI-3 review: follow an in-app link through the host's router (no reload). */
  navigate?: (href: string) => void;
  /** UI-4: the builder's controls, when the graph is being edited. */
  builder?: GraphBuilder;
}

export const GraphContext = createContext<GraphContextValue>({
  selectedId: undefined,
  select: () => {},
  toggle: () => {},
});

export function useGraph(): GraphContextValue {
  return useContext(GraphContext);
}

/**
 * A link's click inside the graph: it never selects the card under it, and
 * a plain click goes through the host's router when there is one. A click
 * meant for a new tab or window stays the browser's.
 */
export function onLinkClick(
  event: MouseEvent<HTMLAnchorElement>,
  navigate: ((href: string) => void) | undefined,
) {
  event.stopPropagation();
  if (!navigate || event.defaultPrevented || event.button !== 0) return;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  const href = event.currentTarget.getAttribute('href');
  if (!href) return;
  event.preventDefault();
  navigate(href);
}
