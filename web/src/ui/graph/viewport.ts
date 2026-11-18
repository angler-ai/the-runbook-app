// UI-2: open a graph at its top, readable. React Flow's fitView centres the
// whole graph, which on a tall runbook means a tiny graph or a view that
// starts halfway down. Instead: fit the width (never below a readable zoom,
// never above 1), centre horizontally when it fits, and put the first step
// at the top.
export interface Bounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Viewport {
  x: number;
  y: number;
  zoom: number;
}

export interface TopViewportOptions {
  /** Screen pixels kept clear around the graph. */
  padding?: number;
  /** The smallest zoom the first view uses, so card titles stay readable. */
  minZoom?: number;
  /** The largest zoom the first view uses (1: no enlarging small graphs). */
  maxZoom?: number;
  /**
   * For a graph wider than the pane: the graph x to centre (the first
   * step's centre), kept within the graph's edges. Without it the view
   * starts at the graph's left edge.
   */
  anchorX?: number;
}

/**
 * Card titles are 14px; at this zoom they are 12.6px on screen. A graph
 * wider than the pane at this zoom starts at its top, centred on its first
 * step, and pans.
 */
export const READABLE_MIN_ZOOM = 0.9;

export function topAlignedViewport(
  bounds: Bounds,
  pane: { width: number; height: number },
  { padding = 24, minZoom = READABLE_MIN_ZOOM, maxZoom = 1, anchorX }: TopViewportOptions = {},
): Viewport {
  const room = Math.max(pane.width - 2 * padding, 1);
  const fitWidth = bounds.width > 0 ? room / bounds.width : maxZoom;
  const zoom = Math.min(maxZoom, Math.max(minZoom, fitWidth));
  const drawnWidth = bounds.width * zoom;
  let x: number;
  if (drawnWidth <= room) {
    x = (pane.width - drawnWidth) / 2 - bounds.x * zoom;
  } else {
    const leftEdge = padding - bounds.x * zoom;
    const rightEdge = pane.width - padding - (bounds.x + bounds.width) * zoom;
    x =
      anchorX === undefined
        ? leftEdge
        : Math.min(leftEdge, Math.max(rightEdge, pane.width / 2 - anchorX * zoom));
  }
  const y = padding - bounds.y * zoom;
  return { x: Math.round(x), y: Math.round(y), zoom };
}

/** The box around positioned nodes of a fixed size. */
export function nodeBounds(
  nodes: { position: { x: number; y: number } }[],
  size: { width: number; height: number },
): Bounds {
  if (nodes.length === 0) return { x: 0, y: 0, width: 0, height: 0 };
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const { position } of nodes) {
    minX = Math.min(minX, position.x);
    minY = Math.min(minY, position.y);
    maxX = Math.max(maxX, position.x + size.width);
    maxY = Math.max(maxY, position.y + size.height);
  }
  return { x: minX, y: minY, width: maxX - minX, height: maxY - minY };
}
