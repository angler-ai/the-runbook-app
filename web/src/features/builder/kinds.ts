// UI-4: what the builder's palette offers, in one place: each kind's name,
// icon, one-line description and the YAML a new step of that kind starts
// with. New steps carry only what they need: no `actor` (LANG7: a step is
// "Anyone" unless the runbook's defaults say otherwise) and no title (the
// card shows the title derived from the ID until one is typed).
import {
  CircleCheck,
  CircleStop,
  Clock,
  Folder,
  GitBranch,
  Radio,
  Repeat,
  RotateCw,
  ShieldCheck,
  Sliders,
  Split,
  SquareCheck,
  Workflow,
  type LucideIcon,
} from 'lucide-react';
import type { OutlineNodeKind } from '../../ui/types';
import { uniqueId } from './ids';

export type PaletteKind =
  | 'task'
  | 'check'
  | 'approval'
  | 'wait_signal'
  | 'wait_time'
  | 'group'
  | 'parallel'
  | 'switch'
  | 'foreach'
  | 'loop'
  | 'call'
  | 'set'
  | 'end';

export interface CreateContext {
  /** Every step ID already written; new IDs are added as they are taken. */
  taken: Set<string>;
  /** A published runbook a new call could point at (the picker changes it). */
  callTarget?: string;
}

export interface PaletteEntry {
  kind: PaletteKind;
  /** The outline kind a step of this entry reads as. */
  outlineKind: OutlineNodeKind;
  label: string;
  description: string;
  icon: LucideIcon;
  /** The ID a new step starts from (made unique). */
  base: string;
  create(ctx: CreateContext): Record<string, unknown>;
}

function take(ctx: CreateContext, base: string): string {
  const id = uniqueId(base, ctx.taken);
  ctx.taken.add(id);
  return id;
}

export const PALETTE: readonly PaletteEntry[] = [
  {
    kind: 'task',
    outlineKind: 'task',
    label: 'Task',
    description: 'Work a person or an agent does, with instructions.',
    icon: SquareCheck,
    base: 'task',
    create: (ctx) => ({ id: take(ctx, 'task') }),
  },
  {
    kind: 'check',
    outlineKind: 'task',
    label: 'Check',
    description: 'A task that ends with a pass, warn or fail verdict.',
    icon: CircleCheck,
    base: 'check',
    create: (ctx) => ({ id: take(ctx, 'check'), check: true }),
  },
  {
    kind: 'approval',
    outlineKind: 'approval',
    label: 'Approval',
    description: 'People choose an option before the run goes on.',
    icon: ShieldCheck,
    base: 'approval',
    create: (ctx) => ({
      id: take(ctx, 'approval'),
      approval: { prompt: 'Approve?', options: ['approve', 'reject'] },
    }),
  },
  {
    kind: 'wait_signal',
    outlineKind: 'wait',
    label: 'Wait for a signal',
    description: 'Pauses until a customer, an API or a person sends a named signal.',
    icon: Radio,
    base: 'wait_for_signal',
    create: (ctx) => {
      const id = take(ctx, 'wait_for_signal');
      return { id, wait: { signal: 'ready' } };
    },
  },
  {
    kind: 'wait_time',
    outlineKind: 'wait',
    label: 'Wait for a time',
    description: 'Pauses for a duration, or until a date and time.',
    icon: Clock,
    base: 'wait',
    create: (ctx) => ({ id: take(ctx, 'wait'), wait: { for: '1h' } }),
  },
  {
    kind: 'group',
    outlineKind: 'group',
    label: 'Group',
    description: 'Steps in order, with optional on-failure and finally steps.',
    icon: Folder,
    base: 'group',
    create: (ctx) => {
      const id = take(ctx, 'group');
      return { id, steps: [{ id: take(ctx, 'task') }] };
    },
  },
  {
    kind: 'parallel',
    outlineKind: 'parallel',
    label: 'Parallel (any order)',
    description: 'Steps that can run at the same time, in any order.',
    icon: Split,
    base: 'parallel',
    create: (ctx) => {
      const id = take(ctx, 'parallel');
      return { id, parallel: [{ id: take(ctx, 'task') }, { id: take(ctx, 'task') }] };
    },
  },
  {
    kind: 'switch',
    outlineKind: 'switch',
    label: 'Branch (switch)',
    description: 'Takes the first case whose condition holds, else the default.',
    icon: GitBranch,
    base: 'decision',
    create: (ctx) => {
      const id = take(ctx, 'decision');
      return {
        id,
        switch: {
          cases: [{ when: '${{ true }}', steps: [{ id: take(ctx, 'task') }] }],
          default: { steps: [] },
        },
      };
    },
  },
  {
    kind: 'foreach',
    outlineKind: 'foreach',
    label: 'For each',
    description: 'Repeats its steps for every item in a list.',
    icon: Repeat,
    base: 'for_each',
    create: (ctx) => {
      const id = take(ctx, 'for_each');
      return {
        id,
        foreach: {
          items: '${{ inputs.items }}',
          as: 'item',
          max_items: 10,
          steps: [{ id: take(ctx, 'task') }],
        },
      };
    },
  },
  {
    kind: 'loop',
    outlineKind: 'loop',
    label: 'Loop',
    description: 'Repeats its steps until (or while) a condition holds.',
    icon: RotateCw,
    base: 'repeat',
    create: (ctx) => {
      const id = take(ctx, 'repeat');
      const check = take(ctx, 'check');
      return {
        id,
        loop: {
          until: `\${{ steps.${check}.verdict == 'pass' }}`,
          max_iterations: 3,
          steps: [{ id: check, check: true }],
        },
      };
    },
  },
  {
    kind: 'call',
    outlineKind: 'call',
    label: 'Call another runbook',
    description: 'Runs a published runbook of this project as a child run.',
    icon: Workflow,
    base: 'call',
    create: (ctx) => ({
      id: take(ctx, 'call'),
      call: ctx.callTarget ? { runbook: ctx.callTarget } : {},
    }),
  },
  {
    kind: 'set',
    outlineKind: 'set',
    label: 'Set',
    description: 'Sets run fields or variables.',
    icon: Sliders,
    base: 'set',
    create: (ctx) => ({ id: take(ctx, 'set'), set: {} }),
  },
  {
    kind: 'end',
    outlineKind: 'end',
    label: 'End',
    description: 'Ends the run here with a status.',
    icon: CircleStop,
    base: 'end',
    create: (ctx) => ({ id: take(ctx, 'end'), end: { status: 'succeeded' } }),
  },
];

export function paletteEntry(kind: PaletteKind): PaletteEntry {
  const entry = PALETTE.find((e) => e.kind === kind);
  if (!entry) throw new Error(`unknown palette kind ${kind}`);
  return entry;
}
