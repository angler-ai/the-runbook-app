// UI-4: where a step's body lives in the YAML source. Outline paths are
// JSON pointers to the step's map (`/spec/steps/2`), so the list a step sits
// in, and the lists a container holds, can be named without the compiler.
// The builder inserts into these lists; the graph draws a "+" for each.
import type { OutlineBranch, OutlineNode } from '../types';

/** The runbook's own two lists. */
export const TOP_STEPS = '/spec/steps';
export const TOP_FINALLY = '/spec/finally';

/** The list a container's children are in (group, parallel, foreach, loop). */
export function childBody(node: OutlineNode): string | null {
  switch (node.kind) {
    case 'group':
      return `${node.path}/steps`;
    case 'parallel':
      return `${node.path}/parallel`;
    case 'foreach':
      return `${node.path}/foreach/steps`;
    case 'loop':
      return `${node.path}/loop/steps`;
    default:
      return null;
  }
}

/**
 * The list a branch's steps are in: a switch case (counted among the cases
 * only), the switch default, or a group's catch and finally.
 */
export function branchBody(node: OutlineNode, branch: OutlineBranch, index: number): string | null {
  const cases = (node.branches ?? []).slice(0, index).filter((b) => branchKind(b) === 'case');
  switch (branchKind(branch)) {
    case 'case':
      return node.kind === 'switch' ? `${node.path}/switch/cases/${cases.length}/steps` : null;
    case 'default':
      return node.kind === 'switch' ? `${node.path}/switch/default/steps` : null;
    case 'catch':
      return node.kind === 'group' ? `${node.path}/catch` : null;
    case 'finally':
      return node.kind === 'group' ? `${node.path}/finally` : null;
  }
}

/** A branch's role, also for older outlines that carry only a label. */
export function branchKind(branch: OutlineBranch): 'case' | 'default' | 'catch' | 'finally' {
  if (branch.kind) return branch.kind;
  if (branch.label === 'default' || branch.label === 'catch' || branch.label === 'finally')
    return branch.label;
  return 'case';
}

/** The list a step is in and its index there, from its pointer. */
export function parentBody(path: string): { body: string; index: number } | null {
  const cut = path.lastIndexOf('/');
  if (cut <= 0) return null;
  const index = Number(path.slice(cut + 1));
  if (!Number.isInteger(index) || index < 0) return null;
  return { body: path.slice(0, cut), index };
}
