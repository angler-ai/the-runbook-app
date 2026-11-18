// UI-2: the words the graph uses for each kind of step. Pure, so the node
// cards, edges and side panel agree and tests can pin them.
import type {
  OutlineBranch,
  OutlineJoin,
  OutlineNode,
  OutlineNodeKind,
  OutlineWait,
} from '../types';

export const KIND_LABELS: Record<OutlineNodeKind, string> = {
  task: 'Task',
  group: 'Group',
  parallel: 'Parallel',
  foreach: 'For each',
  loop: 'Loop',
  switch: 'Decision',
  wait: 'Wait',
  approval: 'Approval',
  set: 'Set',
  end: 'End',
  call: 'Call',
};

/** Container kinds draw their children below them and can collapse. */
export function isContainer(node: OutlineNode): boolean {
  return (
    (node.children?.length ?? 0) > 0 || (node.branches ?? []).some((b) => b.children.length > 0)
  );
}

export function joinLabel(join: OutlineJoin): string {
  switch (join.kind) {
    case 'all':
      return 'Join: all';
    case 'any':
      return 'Join: any';
    case 'at_least':
      return `Join: at least ${join.count}`;
  }
}

/** What the join means, for the side panel. */
export function joinMeaning(join: OutlineJoin): string {
  switch (join.kind) {
    case 'all':
      return 'Waits for every branch.';
    case 'any':
      return 'Continues when the first branch succeeds; the others are cancelled.';
    case 'at_least':
      return `Continues when ${join.count} branches succeed; the others are cancelled.`;
  }
}

/** A wait's one-line summary; the value (signal, duration, time) is last. */
export function waitLabel(wait: OutlineWait): { prefix: string; value: string } {
  switch (wait.kind) {
    case 'signal':
      return { prefix: 'Waits for signal', value: wait.signal };
    case 'for':
      return { prefix: 'Waits', value: wait.duration };
    case 'until':
      return { prefix: 'Waits until', value: wait.until };
  }
}

/** Edge and panel label of a branch: a switch case's condition, else its role. */
export function branchLabel(branch: OutlineBranch): string {
  switch (branch.kind) {
    case 'catch':
      return 'on failure';
    case 'finally':
      return 'finally';
    case 'default':
      return 'default';
    default:
      return branch.label;
  }
}

/** Shorten long edge labels; the full text stays in the side panel. */
export function shorten(text: string, max = 32): string {
  return text.length <= max ? text : `${text.slice(0, max - 1).trimEnd()}…`;
}

export function actorLabel(actor: OutlineNode['actor']): string | undefined {
  switch (actor) {
    case 'agent':
      return 'Agent';
    case 'human':
      return 'Human';
    case 'any':
      return 'Anyone';
    default:
      return undefined;
  }
}

/** The card title: the step's own title, else words for untitled steps. */
export function nodeTitle(node: OutlineNode): string {
  if (node.title) return node.title;
  if (node.kind === 'set' && node.set) {
    const names = [...(node.set.fields ?? []), ...(node.set.vars ?? [])];
    if (names.length > 0) return `Set ${names.join(', ')}`;
  }
  if (node.kind === 'end' && node.end) return `End: ${node.end.status}`;
  return node.id;
}

/** Case count words for a decision card. */
export function switchSummary(node: OutlineNode): string | undefined {
  const branches = node.branches ?? [];
  const cases = branches.filter((b) => b.kind === 'case' || (!b.kind && b.label !== 'default'));
  if (branches.length === 0) return undefined;
  const plural = cases.length === 1 ? 'case' : 'cases';
  return `${cases.length} ${plural} + default`;
}

/** Steps hidden when a container collapses. */
export function descendantCount(node: OutlineNode): number {
  let count = 0;
  const walk = (n: OutlineNode) => {
    for (const child of n.children ?? []) {
      count++;
      walk(child);
    }
    for (const branch of n.branches ?? []) {
      for (const child of branch.children) {
        count++;
        walk(child);
      }
    }
  };
  walk(node);
  return count;
}
