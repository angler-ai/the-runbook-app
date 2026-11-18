// ONB-VIEW: model and derivation for the Checklist view of a run.
// Pure functions computing progress, "next up" ready/waiting items,
// and top-level sections (parallel groups, foreach, taken switch branch).
import type { DirectoryNames } from '../context/names';
import type { Outline, OutlineNode } from '../../ui/types';
import { isUserId } from '../../ui/execution/names';
import { fireTime, recipientsLabel } from '../home/model';
import {
  indexRecords,
  iterationKeys,
  iterationName,
  keyedForeaches,
  takenBranch,
  takenOverBy,
  type RunDocument,
  type RunRecord,
  type ScopeEntry,
  type Section as StepSection,
  type StepView,
} from './overlay';

export const STRUCTURAL_KINDS = new Set([
  'set',
  'end',
  'group',
  'parallel',
  'foreach',
  'loop',
  'switch',
]);

/** Count leaf steps that people, agents, signals or approvals complete. */
export function isLeafStep(kind?: string | null): boolean {
  return !kind || !STRUCTURAL_KINDS.has(kind);
}

function sameScope(a: readonly ScopeEntry[], b: readonly ScopeEntry[]): boolean {
  return (
    a.length === b.length &&
    a.every((entry, i) => entry.container === b[i]?.container && entry.key === b[i]?.key)
  );
}

export interface ChecklistProgress {
  done: number;
  total: number;
  skipped: number;
  cancelled: number;
}

export function computeChecklistProgress(
  doc: RunDocument,
  outline: Outline | null,
): ChecklistProgress {
  const records = indexRecords(doc, keyedForeaches(outline));
  let done = 0;
  let total = 0;
  let skipped = 0;
  let cancelled = 0;

  if (outline && outline.steps.length > 0) {
    const walk = (nodes: OutlineNode[], scope: ScopeEntry[]) => {
      for (const node of nodes) {
        if (node.kind === 'set' || node.kind === 'end') {
          continue;
        }
        if (node.kind === 'parallel' || node.kind === 'group') {
          walk(node.children ?? [], scope);
          for (const branch of node.branches ?? []) {
            walk(branch.children, scope);
          }
        } else if (node.kind === 'switch') {
          const rec = records.find((r) => r.nodeId === node.id && sameScope(r.scope, scope));
          const taken = takenBranch(node, rec, records, scope);
          if (taken) {
            walk(taken.children, scope);
          }
        } else if (node.kind === 'foreach' || node.kind === 'loop') {
          const keys = iterationKeys(records, node.id, scope);
          if (keys.length > 0) {
            for (const key of keys) {
              walk(node.children ?? [], [...scope, { container: node.id, key }]);
            }
          } else {
            walk(node.children ?? [], scope);
          }
        } else {
          // Leaf step
          total += 1;
          const rec = records.find((r) => r.nodeId === node.id && sameScope(r.scope, scope));
          const status = rec?.section.status;
          if (status === 'succeeded') {
            done += 1;
          } else if (status === 'skipped') {
            skipped += 1;
          } else if (status === 'cancelled') {
            cancelled += 1;
          }
        }
      }
    };
    walk(outline.steps, []);
    walk(outline.finally ?? [], []);
  } else {
    for (const section of doc.sections) {
      if (!isLeafStep(section.kind)) continue;
      total += 1;
      if (section.status === 'succeeded') {
        done += 1;
      } else if (section.status === 'skipped') {
        skipped += 1;
      } else if (section.status === 'cancelled') {
        cancelled += 1;
      }
    }
  }

  return { done, total, skipped, cancelled };
}

export function waitingOnLabel(
  step?: StepView,
  section?: StepSection,
  node?: OutlineNode,
  names?: Pick<DirectoryNames, 'nameOf' | 'teamName'>,
): string {
  const nameOf = names?.nameOf ?? ((id: string) => id);
  const teamName = names?.teamName ?? ((ref: string) => ref);

  // 1. Approvals
  if (step?.kind === 'approval' || section?.kind === 'approval' || node?.kind === 'approval') {
    const recipients = section?.approval?.eligible_recipients ?? step?.eligible_recipients;
    let who = '';
    if (recipients && recipients.length > 0) {
      who = recipientsLabel(recipients, nameOf, teamName);
    } else if (node?.approval?.approvers && node.approval.approvers.length > 0) {
      who = node.approval.approvers
        .map((a) => {
          if (a.startsWith('team:')) return `team ${teamName(a.slice(5))}`;
          if (a.startsWith('user:')) return nameOf(a.slice(5));
          return a;
        })
        .join(', ');
    }
    return who ? `Needs approval from ${who}` : 'Needs approval';
  }

  // 2. Wait steps (signals or timers)
  if (step?.kind === 'wait' || section?.kind === 'wait' || node?.kind === 'wait') {
    const signal =
      step?.wait?.signal ??
      (node?.wait?.kind === 'signal' ? node.wait.signal : undefined);
    if (signal) {
      return `Waiting for ${signal}`;
    }
    if (step?.wait?.until_ms != null) {
      return `Waiting for timer (${fireTime(step.wait.until_ms, Date.now())})`;
    }
    return 'Waiting for signal';
  }

  // 3. Tasks
  const status = section?.status ?? step?.status;
  const actor = section?.actor ?? step?.actor;

  // If claimed and working:
  if (status === 'claimed') {
    if (actor?.kind === 'agent') {
      return actor.name;
    }
    if (actor?.kind === 'human') {
      return nameOf(actor.user_id);
    }
  }

  // If not claimed or ready: check assignee first
  const recipients =
    step?.eligible_recipients && step.eligible_recipients.length > 0
      ? step.eligible_recipients
      : section?.assignee?.recipients && section.assignee.recipients.length > 0
        ? section.assignee.recipients
        : step?.assignees && step.assignees.length > 0
          ? step.assignees
          : undefined;
  if (recipients && recipients.length > 0) {
    return recipientsLabel(recipients, nameOf, teamName);
  }

  // Check assignee string (e.g. from section.assignee.to or outline node)
  const assigneeStr = section?.assignee?.to ?? node?.assignee;
  if (assigneeStr) {
    if (assigneeStr.startsWith('team:')) {
      return `team ${teamName(assigneeStr.slice(5))}`;
    }
    if (assigneeStr.startsWith('user:')) {
      return nameOf(assigneeStr.slice(5));
    }
    return assigneeStr;
  }

  // Check actor kind
  const actorKind = node?.actor ?? step?.actor_kind;
  if (actorKind === 'human') {
    return 'A person';
  }

  return 'Any agent';
}

export function whoCompleted(
  section?: StepSection,
  names?: Pick<DirectoryNames, 'nameOf' | 'teamName'>,
): string | null {
  if (!section) return null;
  const nameOf = names?.nameOf ?? ((id: string) => id);

  const taken = takenOverBy(section, nameOf);
  if (taken) {
    return `Taken over by ${taken}`;
  }
  if (section.actor?.kind === 'human') {
    return nameOf(section.actor.user_id);
  }
  if (section.actor?.kind === 'agent') {
    return section.actor.name;
  }
  if (section.outputs?.decided_by) {
    const by = String(section.outputs.decided_by);
    return `Approved by ${isUserId(by) ? nameOf(by) : by}`;
  }
  return null;
}

export function formatTime(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

export interface ChecklistStepItem {
  id: string;
  stepRunId: string;
  path: string;
  title: string;
  kind?: string;
  status: string;
  verdict?: 'pass' | 'warn' | 'fail' | null;
  actor?: StepSection['actor'];
  assignee?: string;
  completedAtMs?: number | null;
  section?: StepSection;
  step?: StepView;
  node?: OutlineNode;
}

export type ChecklistSection =
  | {
      kind: 'single';
      title: string;
      item: ChecklistStepItem;
    }
  | {
      kind: 'parallel';
      title: string;
      badge: 'Any order';
      items: ChecklistStepItem[];
    }
  | {
      kind: 'foreach';
      title: string;
      items: {
        key: string;
        label: string;
        steps: ChecklistStepItem[];
      }[];
    }
  | {
      kind: 'switch';
      title: string;
      branchLabel?: string;
      items: ChecklistStepItem[];
    }
  | {
      kind: 'group';
      title: string;
      items: ChecklistStepItem[];
    };

function makeStepItem(
  node: OutlineNode | undefined,
  record: RunRecord | undefined,
  defaultPath: string,
): ChecklistStepItem {
  const section = record?.section;
  const step = record?.step;
  const path = section?.path ?? defaultPath;
  const id = node?.id ?? section?.step_run_id ?? path;
  const title = section?.title || node?.title || id;
  const kind = section?.kind || node?.kind;
  const status = section?.status ?? 'pending';

  return {
    id,
    stepRunId: section?.step_run_id ?? id,
    path,
    title,
    kind,
    status,
    verdict: section?.verdict,
    actor: section?.actor,
    assignee: (node?.assignee ?? section?.assignee?.to) ?? undefined,
    completedAtMs: section?.completed_at_ms,
    section,
    step,
    node,
  };
}

export function buildChecklistSections(
  doc: RunDocument,
  outline: Outline | null,
): ChecklistSection[] {
  const records = indexRecords(doc, keyedForeaches(outline));
  const sections: ChecklistSection[] = [];

  if (outline && outline.steps.length > 0) {
    const processTopNode = (node: OutlineNode) => {
      if (node.kind === 'set' || node.kind === 'end') {
        return;
      }
      const record = records.find((r) => r.nodeId === node.id && sameScope(r.scope, []));
      const title = record?.section.title || node.title || node.id;

      if (node.kind === 'parallel') {
        const items = (node.children ?? [])
          .filter((c) => isLeafStep(c.kind))
          .map((child) => {
            const childRec = records.find((r) => r.nodeId === child.id && sameScope(r.scope, []));
            return makeStepItem(child, childRec, child.path);
          });
        sections.push({
          kind: 'parallel',
          title,
          badge: 'Any order',
          items,
        });
      } else if (node.kind === 'foreach') {
        const keys = iterationKeys(records, node.id, []);
        const items = keys.map((key) => {
          const label =
            iterationName(key, record?.step?.iterations?.labels, Boolean(node.foreach?.key)) ??
            `Item ${Number(key) + 1}`;
          const steps = (node.children ?? [])
            .filter((c) => isLeafStep(c.kind))
            .map((child) => {
              const childRec = records.find(
                (r) =>
                  r.nodeId === child.id &&
                  sameScope(r.scope, [{ container: node.id, key }]),
              );
              return makeStepItem(child, childRec, child.path);
            });
          return { key, label, steps };
        });
        sections.push({
          kind: 'foreach',
          title,
          items,
        });
      } else if (node.kind === 'switch') {
        const taken = takenBranch(node, record, records, []);
        if (taken) {
          const items = taken.children
            .filter((c) => isLeafStep(c.kind))
            .map((child) => {
              const childRec = records.find((r) => r.nodeId === child.id && sameScope(r.scope, []));
              return makeStepItem(child, childRec, child.path);
            });
          sections.push({
            kind: 'switch',
            title,
            branchLabel: taken.label,
            items,
          });
        }
      } else if (node.kind === 'group') {
        const items = (node.children ?? [])
          .filter((c) => isLeafStep(c.kind))
          .map((child) => {
            const childRec = records.find((r) => r.nodeId === child.id && sameScope(r.scope, []));
            return makeStepItem(child, childRec, child.path);
          });
        sections.push({
          kind: 'group',
          title,
          items,
        });
      } else {
        // Single leaf step
        const item = makeStepItem(node, record, node.path);
        sections.push({
          kind: 'single',
          title,
          item,
        });
      }
    };

    for (const node of outline.steps) {
      processTopNode(node);
    }
    for (const node of outline.finally ?? []) {
      processTopNode(node);
    }
  } else {
    // No outline: derive sections from doc.sections
    const stepsById = new Map(doc.header.steps.map((s) => [s.id, s]));
    for (const section of doc.sections) {
      if (!isLeafStep(section.kind)) continue;
      const step = stepsById.get(section.step_run_id);
      const item: ChecklistStepItem = {
        id: section.step_run_id,
        stepRunId: section.step_run_id,
        path: section.path,
        title: section.title || section.path,
        kind: section.kind ?? 'task',
        status: section.status,
        verdict: section.verdict,
        actor: section.actor,
        assignee: section.assignee?.to ?? undefined,
        completedAtMs: section.completed_at_ms,
        section,
        step,
      };
      sections.push({
        kind: 'single',
        title: item.title,
        item,
      });
    }
  }

  return sections;
}

export function extractLeafItems(sections: ChecklistSection[]): ChecklistStepItem[] {
  const items: ChecklistStepItem[] = [];
  for (const s of sections) {
    if (s.kind === 'single') {
      items.push(s.item);
    } else if (s.kind === 'parallel' || s.kind === 'switch' || s.kind === 'group') {
      items.push(...s.items);
    } else if (s.kind === 'foreach') {
      for (const it of s.items) {
        items.push(...it.steps);
      }
    }
  }
  return items;
}
