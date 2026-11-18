import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from 'react';
import {
  ReactFlow,
  ReactFlowProvider,
  Background,
  ControlButton,
  Controls,
  MiniMap,
  Panel,
  useReactFlow,
  useStore,
  type EdgeTypes,
  type NodeTypes,
  type FitViewOptions,
  type Viewport,
} from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { ArrowUpToLine, Map as MapIcon } from 'lucide-react';
import type { Outline, OutlineNode } from '../types';
import { useFillHeight } from './fillHeight';
import {
  layout,
  countNodes,
  NODE_HEIGHT,
  NODE_WIDTH,
  type RunbookGraphEdge,
  type RunbookGraphNode,
} from './layout';
import type { GraphOverlay } from './overlay';
import { RunbookNodeComponent, SlotNodeComponent } from './RunbookNodeComponent';
import { InsertableEdge } from './InsertableEdge';
import { NodeDetails } from './NodeDetails';
import { GraphContext, type GraphBuilder, type GraphContextValue } from './context';
import { nodeBounds, topAlignedViewport } from './viewport';
import { EmptyState } from '../layout/EmptyState';
import { cn } from '../primitives/cn';

export interface RunbookGraphProps {
  outline: Outline;
  className?: string;
  /** Fill the screen's height below the graph's top (the page's main graph). */
  fillViewport?: boolean;
  /** Where a `call` step's child runbook lives; the node links there. */
  callHref?: (slug: string) => string | undefined;
  /** Controlled selection (a step id); omit to let the graph keep its own. */
  selectedId?: string;
  onSelect?: (id: string | undefined) => void;
  /** Container step ids drawn collapsed at first. */
  initialCollapsed?: Iterable<string>;
  /** UI-3: a run's state over the cards (and the edges it travelled). */
  overlay?: GraphOverlay;
  /** UI-3: a step the first view keeps on screen (a run's current step). */
  focusId?: string;
  /** UI-3: the side panel's content for a selected step, instead of its definition. */
  renderDetails?: (node: OutlineNode, close: () => void) => ReactNode;
  /**
   * UI-3 review: follow in-app links (a call's runbook, a child run) through
   * the host's router instead of reloading the app.
   */
  onNavigate?: (href: string) => void;
  /**
   * UI-3 review: remember collapsed containers and the viewport under this
   * key, so a remount (a tab switch on the same page) comes back as it was.
   */
  viewKey?: string;
  /**
   * UI-4: edit the graph. The builder renders a "+" on each edge into a step,
   * an "Add step" pill at the end of each list and each card's actions.
   */
  builder?: GraphBuilder;
  /** UI-4: the side panel's content when no step is selected (runbook settings). */
  idlePanel?: ReactNode;
  /**
   * UI-4: move focus to the panel whenever the selection changes (default).
   * The builder turns it off: a step renamed while its title is typed must
   * not take focus from the field; it focuses the panel on a real selection.
   */
  focusPanelOnSelect?: boolean;
  /**
   * UI-4: bring a step selected off screen into view, at the same zoom.
   * Default: while editing (`builder`). UI-4b: the editor turns it on when
   * it is read-only too (Git-managed, a read-only token, locked YAML), so
   * "Show <step>" in the problems list pans there as well.
   */
  panToSelected?: boolean;
}

function prefersReducedMotion(): boolean {
  return globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
}

interface GraphView {
  collapsed?: readonly string[];
  viewport?: Viewport;
}

/** Collapse state and viewport per view key, for the life of the tab. */
const graphViews = new Map<string, GraphView>();

/** Remember part of a graph's view (exported for tests). */
export function rememberGraphView(key: string, view: GraphView) {
  graphViews.set(key, { ...graphViews.get(key), ...view });
}

/** Forget a graph's remembered view (exported for tests). */
export function forgetGraphView(key: string) {
  graphViews.delete(key);
}

const MINIMAP_KEY = 'rb.graph.minimap';

function readMinimap(): boolean {
  try {
    return window.localStorage.getItem(MINIMAP_KEY) === 'shown';
  } catch {
    return false;
  }
}

function saveMinimap(shown: boolean) {
  try {
    window.localStorage.setItem(MINIMAP_KEY, shown ? 'shown' : 'hidden');
  } catch {
    // Storage off: the choice lasts while the graph is on screen.
  }
}

/** The controls' "fit whole graph" button (the first view is fit-from-top). */
export const GRAPH_FIT_VIEW_OPTIONS: FitViewOptions = {
  minZoom: 0.6,
  padding: 0.2,
};

/** Above this many drawn steps the graph offers a minimap (folded away at first). */
export const MINIMAP_THRESHOLD = 12;
/** Above this many steps the graph is not drawn at all. */
export const MAX_GRAPH_NODES = 500;

const nodeTypes: NodeTypes = {
  runbookNode: RunbookNodeComponent,
  slotNode: SlotNodeComponent,
};

const edgeTypes: EdgeTypes = {
  insertable: InsertableEdge,
};

// React Flow's own chrome, on the design tokens so it follows the theme.
const FLOW_THEME = {
  '--xy-background-color': 'var(--color-canvas)',
  '--xy-background-pattern-dots-color': 'var(--color-border-strong)',
  '--xy-controls-button-background-color': 'var(--color-surface)',
  '--xy-controls-button-background-color-hover': 'var(--color-hover)',
  '--xy-controls-button-color': 'var(--color-fg)',
  '--xy-controls-button-color-hover': 'var(--color-fg)',
  '--xy-controls-button-border-color': 'var(--color-border)',
  '--xy-controls-box-shadow': 'var(--shadow-card)',
  '--xy-minimap-background-color': 'var(--color-surface)',
  '--xy-minimap-mask-background-color': 'color-mix(in srgb, var(--color-fg) 14%, transparent)',
  '--xy-minimap-mask-stroke-color': 'var(--color-accent)',
  '--xy-minimap-node-background-color': 'var(--color-border-strong)',
  '--xy-edge-label-background-color': 'var(--color-canvas)',
  '--xy-edge-label-color': 'var(--color-fg-muted)',
} as CSSProperties;

const RUN_MINIMAP: Partial<Record<string, string>> = {
  running: 'var(--color-accent)',
  waiting: 'var(--color-warning)',
  ready: 'var(--color-warning)',
  succeeded: 'var(--color-mint)',
  failed: 'var(--color-danger)',
  timed_out: 'var(--color-danger)',
  expired: 'var(--color-danger)',
};

/** UI-3: edges into dimmed steps fade; edges the run travelled darken. */
function overlayEdges(edges: RunbookGraphEdge[], overlay: GraphOverlay): RunbookGraphEdge[] {
  return edges.map((edge) => {
    const target = overlay.nodes.get(edge.target);
    const source = overlay.nodes.get(edge.source);
    if (!target) return edge;
    if (target.dimmed) {
      return {
        ...edge,
        style: { ...edge.style, opacity: 0.35 },
        labelStyle: { ...edge.labelStyle, opacity: 0.5 },
      };
    }
    if (target.ran && source?.ran && edge.data?.variant !== 'repeat') {
      return {
        ...edge,
        style: { ...edge.style, stroke: 'var(--color-fg-muted)', strokeWidth: 2 },
        markerEnd:
          typeof edge.markerEnd === 'object'
            ? { ...edge.markerEnd, color: 'var(--color-fg-muted)' }
            : edge.markerEnd,
      };
    }
    return edge;
  });
}

function minimapColor(node: RunbookGraphNode, overlay?: GraphOverlay): string {
  const n = node.data.node;
  const run = overlay?.nodes.get(n.id);
  if (run) return RUN_MINIMAP[run.state] ?? 'var(--color-border-input)';
  if (n.destructive) return 'var(--color-danger)';
  switch (n.kind) {
    case 'task':
      return n.actor === 'human' ? 'var(--color-mint)' : 'var(--color-accent)';
    case 'approval':
      return 'var(--color-warning)';
    case 'wait':
      return 'var(--color-accent-hover)';
    case 'call':
      return 'var(--color-accent)';
    default:
      return 'var(--color-border-input)';
  }
}

/** Find a step by id anywhere in the outline. */
export function findNode(outline: Outline, id: string | undefined): OutlineNode | undefined {
  if (!id) return undefined;
  const search = (nodes: OutlineNode[]): OutlineNode | undefined => {
    for (const n of nodes) {
      if (n.id === id) return n;
      const found =
        search(n.children ?? []) ??
        (n.branches ?? []).reduce<OutlineNode | undefined>(
          (hit, b) => hit ?? search(b.children),
          undefined,
        );
      if (found) return found;
    }
    return undefined;
  };
  return search(outline.steps) ?? search(outline.finally);
}

export function RunbookGraph(props: RunbookGraphProps) {
  const { outline, className } = props;
  // Cap the work: above 500 nodes, render an EmptyState instead of running layout
  if (countNodes(outline) > MAX_GRAPH_NODES) {
    return (
      <div
        data-slot="runbook-graph-empty"
        className={cn('p-8 flex items-center justify-center h-full min-h-[400px]', className)}
      >
        <EmptyState
          title="Too large to draw — use the outline"
          description="Use the outline view instead for large runbooks."
        />
      </div>
    );
  }
  return (
    <ReactFlowProvider>
      <GraphCanvas {...props} />
    </ReactFlowProvider>
  );
}

function GraphCanvas(props: RunbookGraphProps) {
  const {
    outline,
    className,
    fillViewport = false,
    callHref,
    selectedId,
    onSelect,
    initialCollapsed,
    overlay,
    focusId,
    renderDetails,
    onNavigate,
    viewKey,
    builder,
    idlePanel,
    focusPanelOnSelect = true,
    panToSelected,
  } = props;
  const saved = viewKey ? graphViews.get(viewKey) : undefined;
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(
    () => new Set(saved?.collapsed ?? initialCollapsed ?? []),
  );
  const [ownSelection, setOwnSelection] = useState<string | undefined>(undefined);
  // Controlled when the host passes `selectedId` at all: clearing it to
  // undefined closes the panel (it must not fall back to an old selection).
  const controlled = Object.prototype.hasOwnProperty.call(props, 'selectedId');
  const selected = controlled ? selectedId : ownSelection;
  const containerRef = useRef<HTMLDivElement>(null);
  const filledHeight = useFillHeight(containerRef, fillViewport);
  const [showMap, setShowMap] = useState(readMinimap);

  const editing = builder !== undefined;
  const panning = panToSelected ?? editing;
  const {
    nodes,
    edges: laidOut,
    slots,
  } = useMemo(
    () => layout(outline, { collapsed, builder: editing }),
    [outline, collapsed, editing],
  );
  // The layout depends on the outline only: a live update restyles edges
  // and re-renders cards, never moves them.
  const edges = useMemo(() => {
    const styled = overlay ? overlayEdges(laidOut, overlay) : laidOut;
    return editing ? styled.map((edge) => ({ ...edge, type: 'insertable' })) : styled;
  }, [laidOut, overlay, editing]);
  const drawn = useMemo(() => (slots.length > 0 ? [...nodes, ...slots] : nodes), [nodes, slots]);
  const selectedNode = findNode(outline, selected);

  const select = useCallback(
    (id: string | undefined) => {
      setOwnSelection(id);
      onSelect?.(id);
    },
    [onSelect],
  );
  const toggle = useCallback(
    (id: string) => {
      setCollapsed((prev) => {
        const next = new Set(prev);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        if (viewKey) rememberGraphView(viewKey, { collapsed: [...next] });
        return next;
      });
    },
    [viewKey],
  );
  // Focus moves into the panel when it opens or shows another step, so
  // keyboard and screen-reader users land on it (its heading names the
  // step, inside the "Step details" region). Not on mount: a host that
  // starts with a selection should not steal focus from the page.
  const focusPanel = useCallback(() => {
    containerRef.current?.querySelector<HTMLElement>('[data-slot="node-details-heading"]')?.focus();
  }, []);
  const shownBefore = useRef(selected);
  useEffect(() => {
    if (selected === shownBefore.current) return undefined;
    shownBefore.current = selected;
    if (!selected || !focusPanelOnSelect) return undefined;
    const frame = requestAnimationFrame(focusPanel);
    return () => cancelAnimationFrame(frame);
  }, [selected, focusPanel, focusPanelOnSelect]);
  // Back to the step the panel described, for keyboard users (an effect,
  // so `close` reads no ref and a host's panel may call it).
  const [refocus, setRefocus] = useState<string | null>(null);
  useEffect(() => {
    if (!refocus) return undefined;
    const frame = requestAnimationFrame(() => {
      containerRef.current
        ?.querySelector<HTMLElement>(
          `[data-node-id="${CSS.escape(refocus)}"] [data-slot="node-select"]`,
        )
        ?.focus();
      setRefocus(null);
    });
    return () => cancelAnimationFrame(frame);
  }, [refocus]);
  const close = useCallback(() => {
    const id = selected;
    select(undefined);
    if (id) setRefocus(id);
  }, [select, selected]);

  // Esc closes the details panel when focus is in the graph (or nowhere).
  useEffect(() => {
    if (!selected) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      const target = e.target as Node | null;
      if (target === document.body || containerRef.current?.contains(target)) {
        e.preventDefault();
        close();
      }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [selected, close]);

  const context = useMemo<GraphContextValue>(
    () => ({
      selectedId: selected,
      select,
      toggle,
      ...(callHref ? { callHref } : {}),
      ...(overlay ? { overlay } : {}),
      ...(onNavigate ? { navigate: onNavigate } : {}),
      ...(builder ? { builder } : {}),
    }),
    [selected, select, toggle, callHref, overlay, onNavigate, builder],
  );

  // The first view: fit from the top, once per runbook (not on every edit,
  // which would yank the view while someone types in the editor).
  const width = useStore((s) => s.width);
  const height = useStore((s) => s.height);
  const { setViewport, getViewport, setCenter } = useReactFlow();
  // A remembered viewport is the first view (no fit-to-top over it).
  const fittedFor = useRef<string | null>(saved?.viewport ? outline.id : null);
  const toTop = useCallback(
    (focus?: string) => {
      if (width <= 0 || height <= 0) return;
      const first = nodes[0];
      const target = focus ? nodes.find((n) => n.id === focus) : undefined;
      const anchor = target ?? first;
      const view = topAlignedViewport(
        nodeBounds(nodes, { width: NODE_WIDTH, height: NODE_HEIGHT }),
        { width, height },
        anchor ? { anchorX: anchor.position.x + NODE_WIDTH / 2 } : {},
      );
      // UI-3: keep a run's current step in view: when the top-aligned view
      // would leave it below the fold, scroll down to put it a third of
      // the way down the pane.
      if (target) {
        const bottom = view.y + (target.position.y + NODE_HEIGHT) * view.zoom;
        if (bottom > height - 24) view.y = Math.round(height / 3 - target.position.y * view.zoom);
      }
      void setViewport(view);
    },
    [nodes, width, height, setViewport],
  );
  useEffect(() => {
    if (width > 0 && height > 0 && fittedFor.current !== outline.id) {
      fittedFor.current = outline.id;
      toTop(focusId);
    }
  }, [outline.id, width, height, toTop, focusId]);

  // UI-4: a step selected off screen (a new one, one picked from the
  // problems list) is brought into view, at the same zoom. The editor only
  // (review round 1; UI-4b: read-only editors too): the run and runbook
  // pages keep their own view.
  const panTarget = useRef(selected);
  useEffect(() => {
    if (selected === panTarget.current) return;
    panTarget.current = selected;
    if (!panning || !selected || width <= 0 || height <= 0) return;
    const node = nodes.find((n) => n.id === selected);
    if (!node) return;
    const { x, y, zoom } = getViewport();
    const left = node.position.x * zoom + x;
    const top = node.position.y * zoom + y;
    const inView =
      left >= 0 &&
      top >= 0 &&
      left + NODE_WIDTH * zoom <= width &&
      top + NODE_HEIGHT * zoom <= height;
    if (!inView)
      void setCenter(node.position.x + NODE_WIDTH / 2, node.position.y + NODE_HEIGHT / 2, {
        zoom,
        // UI-4b: no animation for people who ask for less motion.
        duration: prefersReducedMotion() ? 0 : 200,
      });
  }, [panning, selected, nodes, width, height, getViewport, setCenter]);

  return (
    <GraphContext.Provider value={context}>
      <div
        ref={containerRef}
        data-slot="runbook-graph"
        className={cn(
          'relative flex h-full min-h-[480px] w-full overflow-hidden rounded-lg border border-border bg-canvas',
          className,
        )}
        style={filledHeight != null ? { height: filledHeight } : undefined}
      >
        {/* The open panel sits after every card in tab order: this first
            stop jumps straight to it. */}
        {selectedNode ? (
          <button
            type="button"
            onClick={focusPanel}
            className="sr-only z-20 rounded-md bg-surface px-3 py-1.5 text-sm font-medium text-fg shadow-card focus:not-sr-only focus:absolute focus:left-3 focus:top-3"
          >
            Go to step details
          </button>
        ) : null}
        <div className="relative min-w-0 flex-1">
          <ReactFlow
            nodes={drawn}
            edges={edges}
            nodeTypes={nodeTypes}
            edgeTypes={edgeTypes}
            style={FLOW_THEME}
            minZoom={0.2}
            maxZoom={1.5}
            nodesDraggable={false}
            nodesConnectable={false}
            nodesFocusable={false}
            edgesFocusable={false}
            elementsSelectable={false}
            // Also gives nodes pointer events (they are neither draggable nor
            // React Flow-selectable): a click anywhere on a card selects it.
            onNodeClick={(event, n) => {
              // UI-4: only a click on a step's card selects it. A slot is not a
              // step, and a click in a menu the card opened (a portal) reaches
              // here through React's tree although it happened elsewhere.
              if (n.type !== 'runbookNode') return;
              const target = event.target as globalThis.Node | null;
              if (target && !(event.currentTarget as HTMLElement).contains(target)) return;
              select(n.id);
            }}
            onPaneClick={() => selected && select(undefined)}
            {...(saved?.viewport ? { defaultViewport: saved.viewport } : {})}
            onMoveEnd={(_, viewport) => {
              if (viewKey) rememberGraphView(viewKey, { viewport });
            }}
            proOptions={{ hideAttribution: true }}
            aria-label={`Graph of ${outline.title}`}
          >
            <Background gap={16} size={1} />
            <Controls showInteractive={false} fitViewOptions={GRAPH_FIT_VIEW_OPTIONS}>
              <ControlButton
                onClick={() => toTop()}
                title="Back to the top"
                aria-label="Back to the top"
              >
                <ArrowUpToLine aria-hidden="true" />
              </ControlButton>
              {nodes.length > MINIMAP_THRESHOLD ? (
                // Folded away at first: open, it covers the bottom-right
                // cards of the first view. The choice is remembered.
                <ControlButton
                  onClick={() => {
                    setShowMap((shown) => {
                      saveMinimap(!shown);
                      return !shown;
                    });
                  }}
                  title={showMap ? 'Hide the overview map' : 'Show the overview map'}
                  aria-label={showMap ? 'Hide the overview map' : 'Show the overview map'}
                  aria-pressed={showMap}
                  className="max-sm:hidden!"
                >
                  <MapIcon aria-hidden="true" />
                </ControlButton>
              ) : null}
            </Controls>
            {nodes.length > MINIMAP_THRESHOLD && showMap && (
              <MiniMap
                pannable
                zoomable
                position="bottom-right"
                nodeColor={(n) =>
                  n.type === 'slotNode'
                    ? 'transparent'
                    : minimapColor(n as RunbookGraphNode, overlay)
                }
                nodeStrokeWidth={0}
                nodeBorderRadius={4}
                ariaLabel="Overview of the whole graph"
                className="hidden! rounded-md border border-border sm:block!"
              />
            )}

            {/* Step budget badge. Not on a phone: floating there, it covered
                the first step's status (the run header already counts steps). */}
            {outline.stepBudget != null && (
              <Panel position="top-right" className="m-3 hidden sm:block">
                <div
                  data-slot="step-budget-badge"
                  className="px-2.5 py-1 bg-surface-raised border border-border rounded-md text-2xs font-mono font-medium text-fg shadow-card flex items-center gap-1.5"
                >
                  <span className="font-sans text-fg-muted font-normal">Step budget:</span>
                  <span>{outline.stepBudget} max</span>
                </div>
              </Panel>
            )}
          </ReactFlow>
        </div>
        {selectedNode ? (
          renderDetails ? (
            renderDetails(selectedNode, close)
          ) : (
            <NodeDetails
              node={selectedNode}
              onClose={close}
              {...(callHref ? { callHref } : {})}
              className="absolute inset-y-0 right-0 z-10 md:static md:z-auto"
            />
          )
        ) : (
          (idlePanel ?? null)
        )}
      </div>
    </GraphContext.Provider>
  );
}
