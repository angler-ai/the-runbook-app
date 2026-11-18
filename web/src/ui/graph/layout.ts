import dagre from '@dagrejs/dagre';
import type { Node, Edge } from '@xyflow/react';
import { MarkerType } from '@xyflow/react';
import type { Outline, OutlineNode } from '../types';
import { branchLabel, descendantCount, isContainer, nodeTitle, shorten } from './labels';
import { branchBody, childBody, TOP_FINALLY, TOP_STEPS } from './bodies';

export const NODE_WIDTH = 256;
export const NODE_HEIGHT = 100;
/** UI-4: the builder's "add a step here" pill at the end of each list. */
export const SLOT_WIDTH = 168;
export const SLOT_HEIGHT = 32;

/**
 * How an edge reads: plain flow, a parallel branch, a decision case, the
 * dashed rose "on failure" edge into a group's catch, the dashed "finally"
 * edge into cleanup, or a loop's "repeat" edge back to its header.
 */
export type EdgeVariant = 'flow' | 'branch' | 'case' | 'catch' | 'finally' | 'repeat' | 'slot';

export interface RunbookNodeData extends Record<string, unknown> {
  node: OutlineNode;
  /** In the runbook's top-level `finally`. */
  isFinally?: boolean;
  /** Inside a group's catch or finally. */
  lane?: 'catch' | 'finally';
  stepBudget?: number | null;
  collapsible: boolean;
  collapsed: boolean;
  /** Steps hidden below this one while collapsed. */
  hiddenCount: number;
}

/** UI-4: a place a new step can go: position `index` of the list at `body`. */
export interface InsertPoint {
  /** JSON pointer of the list in the source (`/spec/steps/2/parallel`). */
  body: string;
  index: number;
}

export interface RunbookEdgeData extends Record<string, unknown> {
  variant: EdgeVariant;
  /** The full label, when the drawn one is shortened. */
  fullLabel?: string;
  /** UI-4 (builder): inserting on this edge puts a step here. */
  insert?: InsertPoint;
  /** UI-4: the first edge into a step draws its "+" (several may share one). */
  showInsert?: boolean;
  /** UI-4: the "+" button's accessible name. */
  insertLabel?: string;
}

export type RunbookGraphNode = Node<RunbookNodeData>;
export type RunbookGraphEdge = Edge<RunbookEdgeData>;

/** UI-4: the end of a list, where the builder offers "Add step". */
export interface SlotData extends Record<string, unknown> {
  slot: InsertPoint & {
    /** Short words on the pill ("Add step", "Add branch"). */
    label: string;
    /** The accessible name, naming the list. */
    description: string;
  };
}

export type SlotGraphNode = Node<SlotData>;

export interface LayoutOptions {
  /** Container ids whose descendants are hidden. */
  collapsed?: ReadonlySet<string>;
  /** UI-4: annotate edges with insert points and add a slot at each list's end. */
  builder?: boolean;
}

interface SubgraphResult {
  entryNodeId: string;
  /** Where flow continues after this step; empty after an unconditional `end`. */
  exitNodeIds: string[];
}

/**
 * Counts total nodes across steps and finally sections recursively.
 */
export function countNodes(outline: Outline): number {
  let count = 0;
  for (const step of [...outline.steps, ...outline.finally]) {
    count += 1 + descendantCount(step);
  }
  return count;
}

/** Every container id in the outline, in tree order. */
export function containerIds(outline: Outline): string[] {
  const out: string[] = [];
  const walk = (n: OutlineNode) => {
    if (isContainer(n)) out.push(n.id);
    for (const child of n.children ?? []) walk(child);
    for (const branch of n.branches ?? []) for (const child of branch.children) walk(child);
  };
  for (const step of [...outline.steps, ...outline.finally]) walk(step);
  return out;
}

const STROKE: Record<EdgeVariant, string> = {
  flow: 'var(--color-border-input)',
  branch: 'var(--color-border-input)',
  case: 'var(--color-accent)',
  catch: 'var(--color-danger)',
  finally: 'var(--color-fg-muted)',
  repeat: 'var(--color-accent)',
  slot: 'var(--color-border-strong)',
};

const DASHED: Partial<Record<EdgeVariant, string>> = {
  catch: '6 4',
  finally: '6 4',
  repeat: '4 4',
  slot: '3 4',
};

const LABEL_FILL: Record<EdgeVariant, string> = {
  flow: 'var(--color-fg-muted)',
  branch: 'var(--color-fg-muted)',
  case: 'var(--color-accent-strong)',
  catch: 'var(--color-danger-text)',
  finally: 'var(--color-fg-muted)',
  repeat: 'var(--color-accent-strong)',
  slot: 'var(--color-fg-muted)',
};

/**
 * Pure, deterministic layout function that converts an Outline into React Flow nodes and edges,
 * laid out with Dagre.
 */
export function layout(
  outline: Outline,
  { collapsed = new Set(), builder = false }: LayoutOptions = {},
): {
  nodes: RunbookGraphNode[];
  edges: RunbookGraphEdge[];
  /** UI-4: the builder's end-of-list slots (empty unless `builder`). */
  slots: SlotGraphNode[];
} {
  const nodes: RunbookGraphNode[] = [];
  const edges: RunbookGraphEdge[] = [];
  const slots: SlotGraphNode[] = [];
  /** Insert points that already have a "+" (several edges can enter one step). */
  const shownInserts = new Set<string>();

  function annotate(edge: RunbookGraphEdge, insert: InsertPoint | undefined, before: OutlineNode) {
    if (!builder || !insert || !edge.data) return edge;
    const key = `${insert.body}#${insert.index}`;
    edge.data.insert = insert;
    // Expressions read as their text (`Error rate in region`), as on the card.
    const title = nodeTitle(before).replace(/\$\{\{\s*([\s\S]*?)\s*\}\}/g, '$1');
    edge.data.insertLabel = `Insert a step before ${title}`;
    if (!shownInserts.has(key)) {
      shownInserts.add(key);
      edge.data.showInsert = true;
    }
    return edge;
  }

  /** The builder's "Add step" pill at the end of `body`, after `sources`. */
  function slot(
    body: string | null,
    size: number,
    sources: string[],
    words: { label: string; description: string },
    first: { variant?: EdgeVariant; label?: string } = {},
  ) {
    if (!builder || !body) return;
    const id = `slot:${body}`;
    slots.push({
      id,
      type: 'slotNode',
      position: { x: 0, y: 0 },
      initialWidth: SLOT_WIDTH,
      initialHeight: SLOT_HEIGHT,
      draggable: false,
      selectable: false,
      data: { slot: { body, index: size, ...words } },
    });
    for (const source of sources) {
      const edge = createEdge(source, id, first.variant ?? 'slot', first.label);
      edge.markerEnd = undefined;
      edges.push(edge);
    }
  }

  function createEdge(
    source: string,
    target: string,
    variant: EdgeVariant = 'flow',
    label?: string,
    id?: string,
  ): RunbookGraphEdge {
    const shown = label === undefined ? undefined : shorten(label);
    const dash = DASHED[variant];
    return {
      id: id ?? `e-${source}-${target}-${edges.length}`,
      source,
      target,
      type: 'smoothstep',
      ...(shown === undefined ? {} : { label: shown }),
      data: { variant, ...(shown !== label ? { fullLabel: label } : {}) },
      className: `rb-edge-${variant}`,
      style: {
        stroke: STROKE[variant],
        strokeWidth: variant === 'flow' || variant === 'branch' ? 1.25 : 1.5,
        ...(dash ? { strokeDasharray: dash } : {}),
      },
      labelStyle: {
        fill: LABEL_FILL[variant],
        fontSize: 12,
        fontWeight: 500,
        fontFamily: variant === 'case' ? 'var(--font-mono)' : 'var(--font-sans)',
      },
      labelBgStyle: { fill: 'var(--color-canvas)' },
      labelBgPadding: [6, 3],
      labelBgBorderRadius: 4,
      markerEnd: {
        type: MarkerType.ArrowClosed,
        width: 16,
        height: 16,
        color: STROKE[variant],
      },
    };
  }

  /**
   * Lay out `body` in order after `sources`; returns the body's exits. With
   * `pointer` (the list's place in the source) the builder gets a "+" on
   * each edge into a step and, given `end`, a slot after the last one.
   */
  function sequence(
    body: OutlineNode[],
    sources: string[],
    ctx: Ctx,
    first: { variant?: EdgeVariant; label?: string } = {},
    pointer?: string | null,
    end?: { label: string; description: string },
  ): string[] {
    let prev = sources;
    let last: string | undefined;
    body.forEach((child, i) => {
      const sub = processNode(child, ctx);
      const insert = pointer ? { body: pointer, index: i } : undefined;
      for (const source of prev) {
        edges.push(
          annotate(
            i === 0
              ? createEdge(source, sub.entryNodeId, first.variant, first.label)
              : createEdge(source, sub.entryNodeId),
            insert,
            child,
          ),
        );
      }
      prev = sub.exitNodeIds;
      last = sub.entryNodeId;
    });
    if (end) {
      // After an unconditional end the path stops; the slot still hangs off it.
      const from = body.length === 0 ? sources : prev.length > 0 ? prev : last ? [last] : [];
      slot(pointer ?? null, body.length, from, end, body.length === 0 ? first : {});
    }
    return prev;
  }

  const add = (where: string) => ({
    label: 'Add step',
    description: `Add a step at the end of ${where}`,
  });

  type Ctx = { isFinally: boolean; lane?: 'catch' | 'finally' };

  function processNode(node: OutlineNode, ctx: Ctx): SubgraphResult {
    const collapsible = isContainer(node);
    const isCollapsed = collapsible && collapsed.has(node.id);
    nodes.push({
      id: node.id,
      type: 'runbookNode',
      position: { x: 0, y: 0 },
      // The nodes are controlled without onNodesChange, so React Flow never
      // writes their measured size back; the minimap needs a size to draw them.
      initialWidth: NODE_WIDTH,
      initialHeight: NODE_HEIGHT,
      data: {
        node,
        isFinally: ctx.isFinally,
        ...(ctx.lane ? { lane: ctx.lane } : {}),
        stepBudget: outline.stepBudget,
        collapsible,
        collapsed: isCollapsed,
        hiddenCount: isCollapsed ? descendantCount(node) : 0,
      },
    });

    const self = [node.id];
    if (isCollapsed) return { entryNodeId: node.id, exitNodeIds: self };

    let exitNodeIds: string[] = self;
    const children = node.children ?? [];
    const branches = node.branches ?? [];

    const name = nodeTitle(node);
    const own = childBody(node);
    if (node.kind === 'end') {
      // An unconditional end stops this path; a guarded one may fall through.
      exitNodeIds = node.guard ? self : [];
    } else if (node.kind === 'parallel' && (children.length > 0 || builder)) {
      exitNodeIds = [];
      children.forEach((child, i) => {
        const sub = processNode(child, ctx);
        edges.push(
          annotate(
            createEdge(node.id, sub.entryNodeId, 'branch'),
            own ? { body: own, index: i } : undefined,
            child,
          ),
        );
        exitNodeIds.push(...sub.exitNodeIds);
      });
      slot(
        own,
        children.length,
        self,
        { label: 'Add branch', description: `Add a step to run in parallel in ${name}` },
        { variant: 'branch' },
      );
      if (children.length === 0) exitNodeIds = self;
    } else if (node.kind === 'foreach' && (children.length > 0 || builder)) {
      const alias = node.foreach?.as;
      exitNodeIds = sequence(
        children,
        self,
        ctx,
        { variant: 'branch', label: alias ? `each ${alias}` : 'for each' },
        own,
        add(name),
      );
      if (children.length === 0) exitNodeIds = self;
    } else if (node.kind === 'loop' && (children.length > 0 || builder)) {
      const body = sequence(children, self, ctx, {}, own, add(name));
      if (children.length > 0)
        for (const exitId of body) {
          edges.push(
            createEdge(exitId, node.id, 'repeat', 'repeat', `loop-back-${exitId}-${node.id}`),
          );
        }
      // Flow continues from the body once the loop's condition holds.
      exitNodeIds = body.length > 0 ? body : self;
    } else if (node.kind === 'switch' && branches.length > 0) {
      exitNodeIds = [];
      branches.forEach((branch, index) => {
        const pointer = branchBody(node, branch, index);
        const label = branchLabel(branch);
        if (branch.children.length === 0) {
          // An empty branch (usually `default`) falls through.
          if (!exitNodeIds.includes(node.id)) exitNodeIds.push(node.id);
          sequence([], self, ctx, { variant: 'case', label }, pointer, add(`${name}, ${label}`));
          return;
        }
        exitNodeIds.push(
          ...sequence(
            branch.children,
            self,
            ctx,
            { variant: 'case', label },
            pointer,
            add(`${name}, ${label}`),
          ),
        );
      });
    } else if (node.kind === 'group') {
      const body =
        children.length > 0 || builder ? sequence(children, self, ctx, {}, own, add(name)) : self;
      // An empty group (builder only) continues from itself.
      const groupBody = children.length === 0 ? self : body;
      const catchBranch = branches.find(
        (b) => b.kind === 'catch' || (!b.kind && b.label === 'catch'),
      );
      const finallyBranch = branches.find(
        (b) => b.kind === 'finally' || (!b.kind && b.label === 'finally'),
      );
      let after = groupBody;
      const lanes = node.branches ?? [];
      const laneBody = (b: typeof catchBranch) =>
        b ? branchBody(node, b, lanes.indexOf(b)) : null;
      if (catchBranch && (catchBranch.children.length > 0 || builder)) {
        const recovered = sequence(
          catchBranch.children,
          self,
          { ...ctx, lane: 'catch' },
          { variant: 'catch', label: 'on failure' },
          laneBody(catchBranch),
          add(`${name}, on failure`),
        );
        if (catchBranch.children.length > 0) after = [...groupBody, ...recovered];
      }
      if (finallyBranch && (finallyBranch.children.length > 0 || builder)) {
        const cleaned = sequence(
          finallyBranch.children,
          after.length > 0 ? after : self,
          { ...ctx, lane: 'finally' },
          { variant: 'finally', label: 'finally' },
          laneBody(finallyBranch),
          add(`${name}, finally`),
        );
        if (finallyBranch.children.length > 0) after = cleaned;
      }
      exitNodeIds = after;
    }

    return { entryNodeId: node.id, exitNodeIds };
  }

  // Process main steps sequentially
  const mainExits = sequence(
    outline.steps,
    [],
    { isFinally: false },
    {},
    TOP_STEPS,
    outline.steps.length === 0
      ? { label: 'Add the first step', description: 'Add the first step' }
      : add('the runbook'),
  );

  // The runbook's finally runs whatever happened, after every path.
  if (outline.finally.length > 0) {
    const last = outline.steps[outline.steps.length - 1];
    const sources = mainExits.length > 0 ? mainExits : last ? [last.id] : [];
    sequence(
      outline.finally,
      sources,
      { isFinally: true },
      { variant: 'finally', label: 'finally' },
      TOP_FINALLY,
      { label: 'Add step', description: 'Add a step at the end of finally' },
    );
  }

  // 2. Dagre deterministic layout. Repeat edges point back up and would
  // only bend the ranks: they are drawn but not laid out.
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({
    rankdir: 'TB',
    nodesep: 48,
    // The builder's "+" sits on the edge just above each step: more room.
    ranksep: builder ? 84 : 64,
  });

  for (const node of nodes) {
    g.setNode(node.id, { width: NODE_WIDTH, height: NODE_HEIGHT });
  }
  for (const node of slots) {
    g.setNode(node.id, { width: SLOT_WIDTH, height: SLOT_HEIGHT });
  }
  for (const edge of edges) {
    if (edge.data?.variant !== 'repeat') g.setEdge(edge.source, edge.target);
  }

  dagre.layout(g);

  for (const node of nodes) {
    const dagreNode = g.node(node.id);
    if (dagreNode) {
      node.position = {
        x: Math.round(dagreNode.x - NODE_WIDTH / 2),
        y: Math.round(dagreNode.y - NODE_HEIGHT / 2),
      };
    }
  }
  for (const node of slots) {
    const dagreNode = g.node(node.id);
    if (dagreNode) {
      node.position = {
        x: Math.round(dagreNode.x - SLOT_WIDTH / 2),
        y: Math.round(dagreNode.y - SLOT_HEIGHT / 2),
      };
    }
  }

  return { nodes, edges, slots };
}
