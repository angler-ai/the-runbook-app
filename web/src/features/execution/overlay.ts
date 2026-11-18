// UI-3: the run page's model. Pure functions from a run's document (its
// header's step views and its sections, in runbook order) and the
// runbook's outline to (1) the graph overlay, one entry per outline node,
// and (2) the Document view's nested sections. Both walk the outline the
// same way, so the graph and the document agree on what ran, what was
// skipped, which branch was taken and which iteration is which.
//
// Step paths (crates/runbook-core execution_document::path_of): a step's
// id, prefixed for each iteration it runs in: `rollout[us-east].deploy`
// (a foreach item key, escaped to [A-Za-z0-9_-] and %XX) and
// `settle#2.check` (a loop's iteration, from 1). Outline ids are unique in
// a runbook, so the last segment names the outline node.
import type { Schemas } from '../../api/client';
import type { Outline, OutlineBranch, OutlineNode } from '../../ui/types';
import type { GraphOverlay, NodeOverlay, RunState } from '../../ui/graph/overlay';
import { stepWaitingOn } from '../home/model';
import type { NameOf } from '../../ui/execution/names';

export type RunDocument = Schemas['ExecutionDocumentView'];
export type Section = Schemas['StepSection'];
export type StepView = Schemas['StepView'];

export interface Names {
  nameOf: NameOf;
  teamName: (reference: string) => string;
}

export interface ScopeEntry {
  /** The foreach or loop's outline id. */
  container: string;
  /** `us-east` for a foreach item, `#2` for a loop's second iteration. */
  key: string;
}

export interface PathSegment {
  id: string;
  key?: string;
}

/** `rollout[us-east].settle#2.check` → rollout/us-east, settle/#2, check. */
export function parseStepPath(path: string): PathSegment[] {
  return path.split('.').map((part) => {
    const item = /^(.*)\[([^\]]*)\]$/.exec(part);
    if (item) return { id: item[1] ?? part, key: item[2] ?? '' };
    const loop = /^(.*)#(\d+)$/.exec(part);
    if (loop) return { id: loop[1] ?? part, key: `#${loop[2]}` };
    return { id: part };
  });
}

/** A foreach item key as written (`us%2Deast` → `us-east`); a loop key as `2`. */
export function keyLabel(key: string): string {
  if (key.startsWith('#')) return key.slice(1);
  try {
    return decodeURIComponent(key);
  } catch {
    return key;
  }
}

/**
 * A foreach iteration's name. Without `key:` the key is the item's index,
 * so the name is the item (from the view's `labels`), or none when the
 * labels do not say. With `key:` the key is the name, digits or not.
 */
export function iterationName(
  key: string,
  labels: readonly string[] | undefined,
  keyed: boolean,
): string | undefined {
  if (!keyed && /^\d+$/.test(key)) return labels?.[Number(key)];
  return keyLabel(key);
}

/** The foreach steps whose iterations are keyed by `key:` (not by index). */
export function keyedForeaches(outline: Outline | null): ReadonlySet<string> {
  const keyed = new Set<string>();
  const walk = (nodes: readonly OutlineNode[]) => {
    for (const node of nodes) {
      if (node.kind === 'foreach' && node.foreach?.key) keyed.add(node.id);
      walk(node.children ?? []);
      for (const branch of node.branches ?? []) walk(branch.children);
    }
  };
  if (outline) {
    walk(outline.steps);
    walk(outline.finally);
  }
  return keyed;
}

export interface RunRecord {
  path: string;
  nodeId: string;
  scope: ScopeEntry[];
  section: Section;
  step?: StepView;
  /** Position in the document (runbook order). */
  order: number;
  /** The iterations it runs in, by name: `us-east`, `attempt 2`. */
  scopeNames: string[];
}

/**
 * Every step run in the document, with its outline node and iteration
 * scope. `keyed` names the foreach steps with a `key:` (from the outline).
 */
export function indexRecords(
  doc: RunDocument,
  keyed: ReadonlySet<string> = new Set(),
): RunRecord[] {
  const steps = new Map(doc.header.steps.map((step) => [step.id, step]));
  const records: RunRecord[] = doc.sections.map((section, order) => {
    const segments = parseStepPath(section.path);
    const last = segments[segments.length - 1];
    const scope = segments
      .slice(0, -1)
      .filter((s): s is Required<PathSegment> => s.key !== undefined)
      .map((s) => ({ container: s.id, key: s.key }));
    const step = steps.get(section.step_run_id);
    return {
      path: section.path,
      nodeId: last?.id ?? section.path,
      scope,
      section,
      ...(step ? { step } : {}),
      order,
      scopeNames: [],
    };
  });
  // Name each iteration from its container's record (a foreach's labels).
  for (const record of records) {
    record.scopeNames = record.scope.map((entry, i) => {
      if (entry.key.startsWith('#')) return `attempt ${keyLabel(entry.key)}`;
      const container = records.find(
        (r) => r.nodeId === entry.container && sameScope(r.scope, record.scope.slice(0, i)),
      );
      return (
        iterationName(entry.key, container?.step?.iterations?.labels, keyed.has(entry.container)) ??
        `item ${Number(entry.key) + 1}`
      );
    });
  }
  return records;
}

function sameScope(a: readonly ScopeEntry[], b: readonly ScopeEntry[]): boolean {
  return (
    a.length === b.length &&
    a.every((entry, i) => entry.container === b[i]?.container && entry.key === b[i]?.key)
  );
}

function startsWith(scope: readonly ScopeEntry[], prefix: readonly ScopeEntry[]): boolean {
  return prefix.every(
    (entry, i) => entry.container === scope[i]?.container && entry.key === scope[i]?.key,
  );
}

/** A container's iteration keys under `outer`, in iteration order. */
export function iterationKeys(
  records: readonly RunRecord[],
  containerId: string,
  outer: readonly ScopeEntry[],
): string[] {
  const keys: string[] = [];
  for (const record of records) {
    if (record.scope.length <= outer.length || !startsWith(record.scope, outer)) continue;
    const next = record.scope[outer.length];
    if (next?.container === containerId && !keys.includes(next.key)) keys.push(next.key);
  }
  return keys;
}

const OPEN = new Set(['ready', 'claimed', 'waiting']);
const TERMINAL_RUN = new Set(['succeeded', 'failed', 'cancelled']);
const CONTAINER_KINDS = new Set(['group', 'parallel', 'foreach', 'loop', 'switch']);

function isOpenLeaf(record: RunRecord): boolean {
  return OPEN.has(record.section.status) && !CONTAINER_KINDS.has(record.section.kind ?? '');
}

/**
 * The iteration a container shows by default: the one holding the first
 * open step, else the latest.
 */
export function defaultIteration(
  records: readonly RunRecord[],
  containerId: string,
  outer: readonly ScopeEntry[],
): string | undefined {
  const keys = iterationKeys(records, containerId, outer);
  const open = records.find(
    (r) =>
      isOpenLeaf(r) &&
      r.scope.length > outer.length &&
      startsWith(r.scope, outer) &&
      r.scope[outer.length]?.container === containerId,
  );
  return open?.scope[outer.length]?.key ?? keys[keys.length - 1];
}

/** Which branch of a switch was taken: the view's `branch`, else what ran. */
export function takenBranch(
  node: OutlineNode,
  record: RunRecord | undefined,
  records: readonly RunRecord[],
  scope: readonly ScopeEntry[],
): OutlineBranch | undefined {
  const branches = node.branches ?? [];
  const choice = record?.step?.branch;
  if (choice) {
    if (choice.kind === 'default') return branches.find((b) => b.kind === 'default');
    return branches.filter((b) => b.kind === 'case' || b.kind === undefined)[choice.index];
  }
  // An older server: the branch with a step that left `pending`.
  const ran = (nodes: OutlineNode[]): boolean =>
    nodes.some(
      (n) =>
        records.some(
          (r) => r.nodeId === n.id && sameScope(r.scope, scope) && r.section.status !== 'pending',
        ) ||
        ran(n.children ?? []) ||
        (n.branches ?? []).some((b) => ran(b.children)),
    );
  return branches.find((b) => ran(b.children));
}

const STATE_LABELS: Record<RunState, string> = {
  pending: 'Pending',
  ready: 'Ready',
  running: 'Running',
  waiting: 'Waiting',
  succeeded: 'Succeeded',
  failed: 'Failed',
  skipped: 'Skipped',
  cancelled: 'Cancelled',
  timed_out: 'Timed out',
  expired: 'Expired',
};

/** A step run's state as the graph shows it. */
export function runState(section: Pick<Section, 'status' | 'kind'>): RunState {
  if (section.status === 'claimed') return 'running';
  // The engine keeps a container `ready` while its children run.
  if (section.status === 'ready' && CONTAINER_KINDS.has(section.kind ?? '')) return 'running';
  return section.status;
}

export function actorOf(actor: Section['actor'], nameOf: NameOf): NodeOverlay['actor'] | undefined {
  if (!actor) return undefined;
  if (actor.kind === 'agent')
    return { kind: 'agent', label: actor.model ? `${actor.name} · ${actor.model}` : actor.name };
  if (actor.kind === 'human') return { kind: 'human', label: nameOf(actor.user_id) };
  return undefined;
}

/** LANG7: who took an agent step over, from the run document's section. */
export function takenOverBy(section: Section | undefined, nameOf: NameOf): string | undefined {
  const by = section?.taken_over?.by;
  if (!by) return undefined;
  return by.kind === 'human' ? nameOf(by.user_id) : by.kind === 'agent' ? by.name : 'The system';
}

export type Selection = Readonly<Record<string, string>>;

interface Ctx {
  scope: ScopeEntry[];
  /** The iteration this subtree belongs to has not started. */
  unstarted: boolean;
  untaken: boolean;
  lane?: 'catch' | 'finally';
  /** A container above was skipped (its `if`, or an actor): its title. */
  skippedWith?: string;
  /** The foreach or loop above finished without one iteration. */
  empty?: 'No items' | 'No iterations';
}

export interface RunOverlay extends GraphOverlay {
  nodes: Map<string, NodeOverlay>;
  /** The record each outline node shows (for the side panel). */
  records: ReadonlyMap<string, RunRecord>;
  /** The run's current step (first open leaf in runbook order). */
  currentId?: string;
}

export interface OverlayOptions {
  selection?: Selection;
  names: Names;
  /** The clock a timer wait's label reads ("Timer · 09:00"); defaults to now. */
  now?: number;
}

/** The overlay for every outline node, with the given iterations picked. */
export function buildOverlay(
  outline: Outline,
  doc: RunDocument,
  { selection = {}, names, now = Date.now() }: OverlayOptions,
): RunOverlay {
  const keyed = keyedForeaches(outline);
  const records = indexRecords(doc, keyed);
  const terminal = TERMINAL_RUN.has(doc.header.status);
  const nodes = new Map<string, NodeOverlay>();
  const shown = new Map<string, RunRecord>();
  let currentId: string | undefined;

  const visit = (list: OutlineNode[], ctx: Ctx) => {
    for (const node of list) {
      const record = ctx.unstarted
        ? undefined
        : records.find((r) => r.nodeId === node.id && sameScope(r.scope, ctx.scope));
      if (record) shown.set(node.id, record);
      const overlay = nodeOverlay(record, ctx, terminal, names, now);
      if (record && !currentId && isOpenLeaf(record)) {
        currentId = node.id;
        overlay.current = true;
      }
      // The engine skips only the step itself: what is inside it stays
      // pending with no record, and is skipped with it.
      const inner: Ctx =
        record?.section.status === 'skipped' && !ctx.skippedWith
          ? { ...ctx, skippedWith: titleOf(node, record) }
          : ctx;

      if ((node.kind === 'foreach' || node.kind === 'loop') && node.children?.length) {
        const keys = ctx.unstarted ? [] : iterationKeys(records, node.id, ctx.scope);
        // Finished without one iteration (an empty list, a `while` false at once).
        const empty =
          !!record &&
          keys.length === 0 &&
          (record.step?.iterations?.total === 0 || record.section.status === 'succeeded');
        if (empty) {
          const words = node.kind === 'foreach' ? 'No items' : 'No iterations';
          overlay.note ??= words;
        }
        const picked =
          selection[node.id] && keys.includes(selection[node.id] ?? '')
            ? selection[node.id]
            : ctx.unstarted
              ? undefined
              : defaultIteration(records, node.id, ctx.scope);
        if (picked !== undefined && keys.length > 0) {
          const index = Math.max(0, keys.indexOf(picked));
          const total =
            node.kind === 'foreach'
              ? (record?.step?.iterations?.total ?? keys.length)
              : (node.loop?.maxIterations ?? keys.length);
          const noun = node.kind === 'foreach' ? (node.foreach?.as ?? 'item') : 'attempt';
          overlay.iteration = {
            label: `${noun} ${index + 1} of ${total}`,
            ...(() => {
              const name =
                node.kind === 'foreach'
                  ? iterationName(picked, record?.step?.iterations?.labels, keyed.has(node.id))
                  : undefined;
              return name ? { detail: name } : {};
            })(),
            index,
            keys,
          };
        }
        nodes.set(node.id, overlay);
        visit(node.children, {
          ...inner,
          scope:
            picked !== undefined ? [...ctx.scope, { container: node.id, key: picked }] : ctx.scope,
          unstarted: ctx.unstarted || picked === undefined,
          ...(empty && !inner.empty
            ? { empty: node.kind === 'foreach' ? 'No items' : 'No iterations' }
            : {}),
        });
        continue;
      }

      nodes.set(node.id, overlay);
      visit(node.children ?? [], inner);
      if (node.kind === 'switch') {
        const taken = takenBranch(node, record, records, ctx.scope);
        for (const branch of node.branches ?? []) {
          visit(branch.children, {
            ...inner,
            untaken: ctx.untaken || (!!taken && branch !== taken),
          });
        }
      } else {
        for (const branch of node.branches ?? []) {
          const lane =
            branch.kind === 'catch' ? 'catch' : branch.kind === 'finally' ? 'finally' : undefined;
          visit(branch.children, lane ? { ...inner, lane } : inner);
        }
      }
    }
  };

  visit(outline.steps, { scope: [], unstarted: false, untaken: false });
  visit(outline.finally, { scope: [], unstarted: false, untaken: false, lane: 'finally' });
  return { nodes, records: shown, ...(currentId ? { currentId } : {}) };
}

function nodeOverlay(
  record: RunRecord | undefined,
  ctx: Ctx,
  terminal: boolean,
  names: Names,
  now: number,
): NodeOverlay {
  const section = record?.section;
  const state: RunState = section ? runState(section) : 'pending';
  const overlay: NodeOverlay = {
    state,
    stateLabel: STATE_LABELS[state],
    ran: state !== 'pending',
  };
  if (section?.title) overlay.title = section.title;
  if (section?.verdict) overlay.verdict = section.verdict;
  const actor = actorOf(section?.actor, names.nameOf);
  if (actor) overlay.actor = actor;
  const taken = takenOverBy(section, names.nameOf);
  if (taken) overlay.takenOverBy = taken;
  if (section?.followup) {
    const escalatedTo = (section.followup.escalated_to ?? [])
      .map((recipient) =>
        recipient.kind === 'user'
          ? names.nameOf(recipient.reference)
          : `team ${names.teamName(recipient.reference)}`,
      )
      .join(', ');
    overlay.followup = {
      remindersSent: section.followup.reminders_sent,
      escalated: section.followup.escalated,
      ...(escalatedTo ? { escalatedTo } : {}),
    };
  }
  if (record?.step && record.step.attempt > 1) overlay.attempt = record.step.attempt;
  if (record?.step?.call?.child_execution_id && record.step.call.started) {
    overlay.child = {
      href: `/x/${record.step.call.child_execution_id}`,
      label: 'Child run',
    };
  }

  if (record && isOpenLeaf(record) && record.step) {
    const title = record.section.title || record.path;
    const wait = stepWaitingOn(record.step, record.section, title, names, now);
    if (wait) overlay.badge = { kind: wait.kind, label: wait.label, title: `${title}` };
  }

  // Dimmed: never ran, and will not (or not now).
  if (state === 'skipped') {
    overlay.dimmed = true;
    overlay.note =
      record?.step?.skip_reason === 'condition'
        ? 'Skipped: condition false'
        : actor
          ? `Skipped by ${actor.label}`
          : 'Skipped';
  } else if (ctx.skippedWith && state === 'pending') {
    overlay.state = 'skipped';
    overlay.stateLabel = STATE_LABELS.skipped;
    overlay.dimmed = true;
    overlay.note = `Skipped with ${ctx.skippedWith}`;
  } else if (ctx.empty && state === 'pending') {
    overlay.dimmed = true;
    overlay.stateLabel = ctx.empty;
    overlay.note = ctx.empty === 'No items' ? 'The list was empty' : 'No iteration ran';
  } else if (ctx.untaken && state === 'pending') {
    overlay.dimmed = true;
    overlay.stateLabel = 'Not taken';
    overlay.note = 'Branch not taken';
  } else if (ctx.lane && state === 'pending') {
    overlay.dimmed = true;
    overlay.stateLabel = terminal ? 'Did not run' : 'Not run';
    overlay.note =
      ctx.lane === 'catch'
        ? terminal
          ? 'Nothing failed'
          : 'Runs if something fails'
        : terminal
          ? 'Did not run'
          : 'Runs at the end';
  } else if (terminal && state === 'pending') {
    overlay.dimmed = true;
    overlay.stateLabel = 'Not reached';
  } else if (state === 'cancelled' && !record?.section.actor) {
    overlay.dimmed = true;
  }
  return overlay;
}

// ---- The Document view

export type DocItem =
  | { kind: 'step'; record: RunRecord; node?: OutlineNode; depth: number; children: DocItem[] }
  | { kind: 'iteration'; key: string; label: string; depth: number; children: DocItem[] }
  | {
      kind: 'folded';
      /**
       * Why these fold: skipped by a condition (or inside a skipped step),
       * not taken, not reached yet, never reached, or a foreach or loop
       * body that had no iteration.
       */
      reason: 'skipped' | 'untaken' | 'upcoming' | 'unreached' | 'empty';
      titles: string[];
      /** A switch case's condition, for "not taken". */
      branch?: string;
      depth: number;
    };

function foldReason(
  record: RunRecord | undefined,
  ctx: { untaken: boolean; lane?: string; skipped?: boolean; empty?: boolean },
  terminal: boolean,
): Extract<DocItem, { kind: 'folded' }>['reason'] | null {
  const status = record?.section.status ?? 'pending';
  // A condition skip folds; a step an actor skipped shows, with who and why
  // (as the Markdown export prints it).
  if (status === 'skipped') return record?.step?.skip_reason === 'condition' ? 'skipped' : null;
  if (status !== 'pending') return null;
  if (ctx.skipped) return 'skipped';
  if (ctx.empty) return 'empty';
  if (ctx.untaken) return 'untaken';
  if (ctx.lane || terminal) return 'unreached';
  return 'upcoming';
}

function fold(items: DocItem[], item: Extract<DocItem, { kind: 'folded' }>) {
  const last = items[items.length - 1];
  if (last?.kind === 'folded' && last.reason === item.reason && last.branch === item.branch) {
    last.titles.push(...item.titles);
  } else {
    items.push(item);
  }
}

function titleOf(node: OutlineNode, record: RunRecord | undefined): string {
  return record?.section.title || node.title || node.id;
}

/**
 * The document's sections, nested as the runbook is (groups, parallel,
 * switches, foreach and loop iterations), with skipped, untaken and
 * not-yet-reached steps folded into one line each. Without an outline,
 * every section in document order, indented by iteration depth.
 */
export function documentItems(outline: Outline | null, doc: RunDocument): DocItem[] {
  const records = indexRecords(doc, keyedForeaches(outline));
  if (!outline) {
    return records.map((record) => ({
      kind: 'step',
      record,
      depth: record.scope.length,
      children: [],
    }));
  }
  const terminal = TERMINAL_RUN.has(doc.header.status);
  const used = new Set<RunRecord>();

  const walk = (
    list: OutlineNode[],
    ctx: {
      scope: ScopeEntry[];
      untaken: boolean;
      lane?: string;
      branch?: string;
      skipped?: boolean;
      empty?: boolean;
    },
    depth: number,
  ): DocItem[] => {
    const items: DocItem[] = [];
    for (const node of list) {
      const record = records.find((r) => r.nodeId === node.id && sameScope(r.scope, ctx.scope));
      if (record) used.add(record);
      const reason = foldReason(record, ctx, terminal);
      if (reason) {
        // The steps inside a folded step fold with it.
        const inside = new Set<string>();
        const collect = (n: OutlineNode) => {
          for (const c of [
            ...(n.children ?? []),
            ...(n.branches ?? []).flatMap((b) => b.children),
          ]) {
            inside.add(c.id);
            collect(c);
          }
        };
        collect(node);
        for (const r of records) {
          if (inside.has(r.nodeId) && startsWith(r.scope, ctx.scope)) used.add(r);
        }
        fold(items, {
          kind: 'folded',
          reason,
          titles: [titleOf(node, record)],
          ...(reason === 'untaken' && ctx.branch ? { branch: ctx.branch } : {}),
          depth,
        });
        continue;
      }
      if (!record) continue;
      const children: DocItem[] = [];
      // Inside a step an actor skipped, nothing ran: it folds as skipped.
      const inner = record.section.status === 'skipped' ? { ...ctx, skipped: true } : ctx;
      if ((node.kind === 'foreach' || node.kind === 'loop') && node.children?.length) {
        const keys = iterationKeys(records, node.id, ctx.scope);
        if (keys.length === 0 && record.section.status !== 'skipped') {
          const finished =
            record.step?.iterations?.total === 0 || record.section.status === 'succeeded';
          if (finished) children.push(...walk(node.children, { ...ctx, empty: true }, depth + 1));
        }
        const total =
          node.kind === 'foreach'
            ? (record.step?.iterations?.total ?? keys.length)
            : (node.loop?.maxIterations ?? keys.length);
        keys.forEach((key, i) => {
          const noun = node.kind === 'foreach' ? (node.foreach?.as ?? 'Item') : 'Attempt';
          const name =
            node.kind === 'foreach'
              ? iterationName(key, record.step?.iterations?.labels, node.foreach?.key != null)
              : undefined;
          const label = name
            ? `${capitalize(noun)} ${name} (${i + 1} of ${total})`
            : `${capitalize(noun)} ${i + 1} of ${total}`;
          children.push({
            kind: 'iteration',
            key,
            label,
            depth: depth + 1,
            children: walk(
              node.children ?? [],
              { ...ctx, scope: [...ctx.scope, { container: node.id, key }] },
              depth + 2,
            ),
          });
        });
      } else {
        children.push(...walk(node.children ?? [], inner, depth + 1));
        if (node.kind === 'switch') {
          const taken = takenBranch(node, record, records, ctx.scope);
          for (const branch of node.branches ?? []) {
            const untaken = !!taken && branch !== taken;
            const sub = walk(
              branch.children,
              {
                ...inner,
                untaken: ctx.untaken || untaken,
                branch: branch.kind === 'default' ? 'default' : branch.label,
              },
              depth + 1,
            );
            children.push(...sub);
          }
        } else {
          for (const branch of node.branches ?? []) {
            const lane = branch.kind ?? branch.label;
            children.push(...walk(branch.children, { ...inner, lane }, depth + 1));
          }
        }
      }
      items.push({ kind: 'step', record, node, depth, children });
    }
    return items;
  };

  const items = [
    ...walk(outline.steps, { scope: [], untaken: false }, 0),
    ...walk(outline.finally, { scope: [], untaken: false, lane: 'finally' }, 0),
  ];
  // Anything the outline did not place (a mismatch) still shows, last.
  for (const record of records) {
    if (!used.has(record)) items.push({ kind: 'step', record, depth: 0, children: [] });
  }
  return items;
}

function capitalize(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

// ---- The outcome line

export interface Outcome {
  /** "Succeeded in 4m 12s", "Running for 3m". */
  headline: string;
  /** "6 of 6 checks passed", "4 of 6 checks passed · 1 warned · 1 failed". */
  checks?: string;
}

/** Elapsed time: `45s`, `4m 12s`, `1h 05m`, `2d 3h`. */
export function duration(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${String(seconds % 60).padStart(2, '0')}s`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${String(minutes % 60).padStart(2, '0')}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

const RUN_WORDS: Record<string, string> = {
  queued: 'Queued',
  running: 'Running',
  waiting: 'Waiting',
  paused: 'Paused',
  succeeded: 'Succeeded',
  failed: 'Failed',
  cancelled: 'Cancelled',
};

export function outcomeOf(doc: RunDocument, now: number): Outcome {
  const header = doc.header;
  const start = header.started_at_ms ?? header.created_at_ms ?? null;
  const end = header.completed_at_ms ?? null;
  const word = RUN_WORDS[header.status] ?? header.status;
  let headline = word;
  if (start != null && end != null) headline = `${word} in ${duration(end - start)}`;
  else if (start != null && header.status !== 'queued')
    headline = `${word} · ${duration(now - start)} so far`;
  const checks = doc.sections.filter((s) => s.verdict && !CONTAINER_KINDS.has(s.kind ?? ''));
  if (checks.length === 0) return { headline };
  const pass = checks.filter((s) => s.verdict === 'pass').length;
  const warn = checks.filter((s) => s.verdict === 'warn').length;
  const fail = checks.filter((s) => s.verdict === 'fail').length;
  const parts = [`${pass} of ${checks.length} ${checks.length === 1 ? 'check' : 'checks'} passed`];
  if (warn) parts.push(`${warn} warned`);
  if (fail) parts.push(`${fail} failed`);
  return { headline, checks: parts.join(' · ') };
}

/** Who worked on the run: agents (with model) and people, in first-seen order. */
export function participants(doc: RunDocument, nameOf: NameOf): string[] {
  const seen: string[] = [];
  for (const section of doc.sections) {
    const actor = actorOf(section.actor, nameOf);
    if (actor && !seen.includes(actor.label)) seen.push(actor.label);
  }
  return seen;
}
