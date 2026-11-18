import type { ComponentType, ReactNode } from 'react';
import { Handle, Position, type NodeProps } from '@xyflow/react';
import {
  AlertCircle,
  ArrowUpRight,
  BellRing,
  Bot,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
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
  TriangleAlert,
  User,
  UserCheck,
  Users,
  Workflow,
} from 'lucide-react';
import type { OutlineNode, OutlineNodeKind } from '../types';
import type { RunbookGraphNode, SlotGraphNode } from './layout';
import { cn } from '../primitives/cn';
import { TemplateText } from './TemplateText';
import { onLinkClick, useGraph } from './context';
import { KIND_LABELS, actorLabel, joinLabel, nodeTitle, switchSummary, waitLabel } from './labels';
import type { NodeOverlay } from './overlay';
import { RunStateChip } from './RunStateChip';
import { WaitBadge } from '../status/WaitBadge';

type Icon = ComponentType<{ className?: string; 'aria-hidden'?: boolean | 'true' | 'false' }>;

const KIND_ICONS: Record<OutlineNodeKind, Icon> = {
  task: SquareCheck,
  group: Folder,
  parallel: Split,
  foreach: Repeat,
  loop: RotateCw,
  switch: GitBranch,
  wait: Clock,
  approval: ShieldCheck,
  set: Sliders,
  end: CircleStop,
  call: Workflow,
};

/** The icon for a step: checks and signal waits get their own. */
export function KindIcon({ node, className }: { node: OutlineNode; className?: string }) {
  if (node.kind === 'task' && node.check)
    return <CircleCheck className={className} aria-hidden="true" />;
  if (node.kind === 'wait' && node.wait?.kind === 'signal')
    return <Radio className={className} aria-hidden="true" />;
  const Icon = KIND_ICONS[node.kind] ?? SquareCheck;
  return <Icon className={className} aria-hidden="true" />;
}

/** The kind word on the card (checks read "Check"). */
export function kindWord(node: OutlineNode): string {
  if (node.kind === 'task' && node.check) return 'Check';
  return KIND_LABELS[node.kind] ?? node.kind;
}

/** Stripe (a fill) and icon colours by kind; mint only ever fills. */
function kindTone(node: OutlineNode): { stripe: string; icon: string } {
  if (node.destructive) return { stripe: 'bg-danger', icon: 'text-danger-text' };
  switch (node.kind) {
    case 'task':
      return node.actor === 'human'
        ? { stripe: 'bg-mint', icon: 'text-mint-text' }
        : { stripe: 'bg-accent', icon: 'text-accent' };
    case 'approval':
      return { stripe: 'bg-warning', icon: 'text-warning-text' };
    case 'wait':
      return { stripe: 'bg-accent-hover', icon: 'text-status-waiting-fg' };
    case 'call':
      return { stripe: 'bg-accent', icon: 'text-accent' };
    case 'end':
      return node.end?.status === 'failed'
        ? { stripe: 'bg-danger', icon: 'text-danger-text' }
        : node.end?.status === 'succeeded'
          ? { stripe: 'bg-mint', icon: 'text-mint-text' }
          : { stripe: 'bg-border-strong', icon: 'text-fg-muted' };
    case 'group':
      return { stripe: 'bg-accent-line', icon: 'text-fg-muted' };
    default:
      return { stripe: 'bg-border-strong', icon: 'text-fg-muted' };
  }
}

const ACTOR_ICONS: Record<string, Icon> = { agent: Bot, human: User, any: Users };

function ActorBadge({ actor }: { actor: NonNullable<OutlineNode['actor']> }) {
  const Icon = ACTOR_ICONS[actor] ?? Users;
  return (
    <span
      data-slot="actor-badge"
      className={cn(
        'inline-flex items-center gap-1 rounded-sm px-1.5 py-px text-2xs font-medium',
        actor === 'agent' && 'bg-accent-subtle text-accent-strong',
        actor === 'human' && 'bg-mint-subtle text-fg',
        actor === 'any' && 'border border-border bg-surface-raised text-fg-muted',
      )}
    >
      <Icon className="size-3" aria-hidden="true" />
      {actorLabel(actor)}
    </span>
  );
}

function Chip({ children }: { children: ReactNode }) {
  return (
    <code className="rounded-sm border border-accent-line bg-accent-subtle px-1 font-mono text-[0.92em] text-accent-strong">
      {children}
    </code>
  );
}

/** The card's one-line summary of what the step does. */
export function NodeSummary({ node }: { node: OutlineNode }) {
  const { callHref, navigate } = useGraph();
  switch (node.kind) {
    case 'wait': {
      if (!node.wait) break;
      const { prefix, value } = waitLabel(node.wait);
      return (
        <>
          {prefix}{' '}
          {node.wait.kind === 'until' ? <TemplateText text={value} /> : <Chip>{value}</Chip>}
        </>
      );
    }
    case 'parallel':
      if (node.join) return <>{joinLabel(node.join)}</>;
      break;
    case 'foreach':
      if (node.foreach)
        return (
          <>
            Each <Chip>{node.foreach.as}</Chip> in <TemplateText text={node.foreach.items} />
          </>
        );
      break;
    case 'loop':
      if (node.loop) {
        const condition = node.loop.until ?? node.loop.while;
        return (
          <>
            {node.loop.until ? 'Until ' : 'While '}
            {condition ? <TemplateText text={condition} /> : null} · max {node.loop.maxIterations}
          </>
        );
      }
      break;
    case 'switch': {
      const summary = switchSummary(node);
      if (summary) return <>{summary}</>;
      break;
    }
    case 'approval':
      if (node.approval) {
        const who = node.approval.approvers.join(', ') || 'anyone';
        const options = node.approval.options.map((o) => o.title).join(' / ');
        return (
          <>
            {who}
            {options ? ` · ${options}` : ''}
          </>
        );
      }
      break;
    case 'call':
      if (node.call) {
        const href = callHref?.(node.call.runbook);
        const name = node.call.version
          ? `${node.call.runbook} v${node.call.version}`
          : node.call.runbook;
        return (
          <>
            Runs{' '}
            {href ? (
              <a
                href={href}
                data-slot="call-link"
                className="nodrag nopan relative z-10 font-medium text-accent underline-offset-2 hover:underline"
                onClick={(e) => onLinkClick(e, navigate)}
              >
                {name}
              </a>
            ) : (
              <span className="font-medium text-fg">{name}</span>
            )}
          </>
        );
      }
      break;
    case 'end':
      if (node.end) return <>{node.end.message ?? `Ends ${node.end.status}`}</>;
      break;
    case 'task': {
      const parts: string[] = [];
      if (node.assignee) parts.push(node.assignee);
      if (node.skills && node.skills.length > 0) parts.push(node.skills.join(', '));
      else if (node.skill) parts.push(node.skill);
      if (node.outputs?.length) parts.push(`→ ${node.outputs.map((o) => o.name).join(', ')}`);
      if (node.checklist?.length) parts.push(`${node.checklist.length}-item checklist`);
      if (parts.length > 0) return <>{parts.join(' · ')}</>;
      // The fallback hint starts with the actor ("Agent · check"), which
      // the card's badge already shows; keep only what it adds.
      if (node.destructive) return <>Destructive</>;
      if (node.check) return <>Check</>;
      if (node.actor) return null;
      break;
    }
    case 'group': {
      const lanes = (node.branches ?? []).map((b) => b.kind ?? b.label);
      if (lanes.length > 0)
        return <>{`With ${lanes.map((l) => (l === 'catch' ? 'catch' : l)).join(' and ')}`}</>;
      break;
    }
    default:
      break;
  }
  return <>{node.hint ?? node.id}</>;
}

/** A title as words: `${{ region }}` chips read as their expression. */
function plainTitle(title: string): string {
  return title.replace(/\$\{\{\s*([\s\S]*?)\s*\}\}/g, '$1');
}

/** UI-3: the card's bottom line during a run: what matters most now. */
function RunFooter({ node, run }: { node: OutlineNode; run: NodeOverlay }) {
  const { overlay, navigate } = useGraph();
  // The pickers are named by the step as written (not the iteration's
  // rendered title, which changes as they are used).
  const stepName = plainTitle(nodeTitle(node));
  const iteration = run.iteration;
  if (iteration) {
    const pick = (delta: number) => {
      const key = iteration.keys[iteration.index + delta];
      if (key !== undefined) overlay?.pickIteration?.(node.id, key);
    };
    return (
      <span data-slot="iteration-nav" className="flex min-w-0 items-center gap-0.5">
        <button
          type="button"
          aria-label={`Previous iteration of ${stepName}`}
          disabled={iteration.index <= 0}
          onClick={(e) => {
            e.stopPropagation();
            pick(-1);
          }}
          className="nodrag nopan relative z-10 rounded-sm p-0.5 text-fg-muted hover:bg-hover hover:text-fg disabled:opacity-40"
        >
          <ChevronLeft className="size-3.5" aria-hidden="true" />
        </button>
        <span className="min-w-0 truncate text-xs text-fg">
          <span className="font-medium">{iteration.label}</span>
          {iteration.detail ? (
            <span className="font-mono text-fg-muted"> · {iteration.detail}</span>
          ) : null}
        </span>
        <button
          type="button"
          aria-label={`Next iteration of ${stepName}`}
          disabled={iteration.index >= iteration.keys.length - 1}
          onClick={(e) => {
            e.stopPropagation();
            pick(1);
          }}
          className="nodrag nopan relative z-10 rounded-sm p-0.5 text-fg-muted hover:bg-hover hover:text-fg disabled:opacity-40"
        >
          <ChevronRight className="size-3.5" aria-hidden="true" />
        </button>
      </span>
    );
  }
  if (run.child) {
    return (
      <a
        href={run.child.href}
        data-slot="child-run-link"
        onClick={(e) => onLinkClick(e, navigate)}
        className="nodrag nopan relative z-10 truncate text-xs font-medium text-accent underline-offset-2 hover:underline"
      >
        {run.child.label} ↗
      </a>
    );
  }
  // A taken-over task is the person's (claimed or done): say so over the
  // "person working" badge, which would name them without the take-over.
  if (run.takenOverBy) {
    return (
      <span className="flex min-w-0 items-center gap-1 truncate text-xs text-fg">
        <UserCheck className="size-3 shrink-0" aria-hidden="true" />
        Taken over by {run.takenOverBy}
      </span>
    );
  }
  if (run.badge) return <WaitBadge kind={run.badge.kind} label={run.badge.label} />;
  if (run.note) return <span className="truncate text-xs text-fg-muted">{run.note}</span>;
  if (run.actor) {
    const Icon = run.actor.kind === 'agent' ? Bot : User;
    return (
      <span className="flex min-w-0 items-center gap-1 text-xs text-fg-muted">
        <Icon className="size-3 shrink-0" aria-hidden="true" />
        <span className={cn('truncate', run.actor.kind === 'agent' && 'font-mono text-2xs')}>
          {run.actor.label}
          {run.attempt ? ` · attempt ${run.attempt}` : ''}
        </span>
      </span>
    );
  }
  return (
    <span className="truncate text-xs text-fg-muted">
      <NodeSummary node={node} />
    </span>
  );
}

/** Card border by run state: what needs eyes stands out, the rest stays quiet. */
function runBorder(run: NodeOverlay): string {
  if (run.dimmed) return 'border-dashed border-border-strong';
  switch (run.state) {
    case 'running':
      return 'border-accent';
    case 'waiting':
    case 'ready':
      return run.current ? 'border-warning' : 'border-border-strong';
    case 'failed':
    case 'timed_out':
    case 'expired':
      return 'border-danger';
    default:
      return 'border-border-strong';
  }
}

/** UI-4: the builder's "Add step" pill at the end of a list. */
export function SlotNodeComponent({ data }: NodeProps<SlotGraphNode>) {
  const { builder } = useGraph();
  return (
    <div data-slot="graph-slot" data-body={data.slot.body} className="nodrag nopan">
      <Handle
        type="target"
        position={Position.Top}
        className="size-2! opacity-0!"
        isConnectable={false}
      />
      {builder?.renderSlot(data.slot)}
    </div>
  );
}

export function RunbookNodeComponent({ data }: NodeProps<RunbookGraphNode>) {
  const { node, isFinally, lane, collapsible, collapsed, hiddenCount } = data;
  const { selectedId, select, toggle, overlay, builder } = useGraph();
  const selected = selectedId === node.id;
  const issues = builder?.issues?.get(node.id) ?? 0;
  const run = overlay?.nodes.get(node.id);
  const tone = kindTone(node);
  const title = run?.title ?? nodeTitle(node);
  const actor = node.kind === 'task' || node.kind === 'approval' ? node.actor : undefined;

  return (
    <div
      data-node-id={node.id}
      data-kind={node.kind}
      data-selected={selected || undefined}
      data-state={run?.state}
      data-dimmed={run?.dimmed || undefined}
      data-current={run?.current || undefined}
      data-issues={issues || undefined}
      className={cn(
        'group/node relative flex h-[100px] w-[256px] overflow-hidden rounded-md border bg-surface text-fg shadow-card transition-[border-color,opacity] duration-300',
        run?.dimmed && 'opacity-55',
        run?.current && !selected && 'ring-2 ring-accent-line',
        selected
          ? 'border-accent ring-2 ring-accent-line'
          : issues > 0
            ? 'border-danger ring-1 ring-danger'
            : run
              ? // A destructive step keeps its red border during a run too.
                node.destructive
                ? run.dimmed
                  ? 'border-dashed border-danger'
                  : 'border-danger'
                : runBorder(run)
              : node.destructive
                ? 'border-danger'
                : isFinally || lane === 'finally'
                  ? 'border-dashed border-border-strong'
                  : lane === 'catch'
                    ? 'border-dashed border-danger'
                    : 'border-border-strong hover:border-border-input',
      )}
    >
      <Handle
        type="target"
        position={Position.Top}
        className="size-2! opacity-0!"
        isConnectable={false}
      />
      <span aria-hidden="true" className={cn('w-1 shrink-0', tone.stripe)} />
      <div className="flex min-w-0 flex-1 flex-col gap-1 px-3 py-2">
        <div className="flex items-center justify-between gap-1.5">
          <span className="flex min-w-0 items-center gap-1.5 text-2xs font-medium uppercase tracking-wider text-fg-muted">
            <KindIcon node={node} className={cn('size-3.5 shrink-0', tone.icon)} />
            <span className="truncate">{kindWord(node)}</span>
          </span>
          <span className="flex shrink-0 items-center gap-1">
            {issues > 0 && (
              <span
                data-slot="issue-badge"
                className="inline-flex items-center gap-1 rounded-sm bg-status-failed-bg px-1.5 py-px text-2xs font-semibold text-status-failed-fg"
              >
                <AlertCircle className="size-3" aria-hidden="true" />
                {issues}
              </span>
            )}
            {node.destructive && (
              <span
                data-slot="destructive-badge"
                className="inline-flex items-center gap-1 rounded-sm bg-status-failed-bg px-1.5 py-px text-2xs font-semibold text-status-failed-fg"
              >
                <TriangleAlert className="size-3" aria-hidden="true" />
                destructive
              </span>
            )}
            {(isFinally || lane) && (
              <span
                data-slot={isFinally ? 'finally-badge' : `${lane}-badge`}
                className={cn(
                  'rounded-sm border border-dashed px-1.5 py-px text-2xs font-medium',
                  lane === 'catch'
                    ? 'border-danger text-danger-text'
                    : 'border-border-strong text-fg-muted',
                )}
              >
                {lane === 'catch' ? 'on failure' : 'finally'}
              </span>
            )}
            {run?.followup && run.followup.remindersSent > 0 ? (
              <span
                data-slot="graph-reminded-badge"
                role="img"
                aria-label={`Reminded ${run.followup.remindersSent} ${run.followup.remindersSent === 1 ? 'time' : 'times'}`}
                title={`Reminded ${run.followup.remindersSent} ${run.followup.remindersSent === 1 ? 'time' : 'times'}`}
                className="inline-flex items-center gap-0.5 rounded-sm bg-status-waiting-bg px-1 py-px text-2xs font-semibold text-status-waiting-fg"
              >
                <BellRing className="size-3" aria-hidden="true" />
                {run.followup.remindersSent > 1 ? `×${run.followup.remindersSent}` : null}
              </span>
            ) : null}
            {run?.followup?.escalated ? (
              <span
                data-slot="graph-escalated-badge"
                role="img"
                aria-label={
                  run.followup.escalatedTo
                    ? `Escalated to ${run.followup.escalatedTo}`
                    : 'Escalated'
                }
                title={
                  run.followup.escalatedTo
                    ? `Escalated to ${run.followup.escalatedTo}`
                    : 'Escalated'
                }
                className="inline-flex items-center gap-0.5 rounded-sm bg-status-paused-bg px-1 py-px text-2xs font-semibold text-status-paused-fg"
              >
                <ArrowUpRight className="size-3" aria-hidden="true" />
                Escalated
              </span>
            ) : null}
            {run ? (
              <RunStateChip state={run.state} label={run.stateLabel} verdict={run.verdict} />
            ) : (
              actor && <ActorBadge actor={actor} />
            )}
            {builder ? (
              <span className="nodrag nopan relative z-10 flex items-center">
                {builder.renderActions(node)}
              </span>
            ) : null}
          </span>
        </div>
        {/* The title is the card's button; its ::after covers the card so
            the whole card selects, while the toggle and link stay on top. */}
        <button
          type="button"
          data-slot="node-select"
          aria-pressed={selected}
          aria-label={`${plainTitle(title)}, ${kindWord(node)}${node.destructive ? ', destructive' : ''}${run ? `, ${run.stateLabel}${run.verdict ? `, verdict ${run.verdict}` : ''}` : ''}${issues > 0 ? `, ${issues} ${issues === 1 ? 'problem' : 'problems'}` : ''}`}
          onClick={() => select(node.id)}
          className={cn(
            'nodrag cursor-pointer text-left text-base font-semibold leading-snug text-fg',
            "after:absolute after:inset-0 after:rounded-md after:content-['']",
            'focus-visible:outline-none focus-visible:after:outline-2 focus-visible:after:outline-offset-2 focus-visible:after:outline-focus focus-visible:after:[outline-style:solid]',
          )}
        >
          <TemplateText text={title} className="line-clamp-2" />
        </button>
        <div className="mt-auto flex min-w-0 items-center justify-between gap-2">
          {run ? (
            <span data-slot="node-run" className="flex min-w-0 items-center">
              <RunFooter node={node} run={run} />
            </span>
          ) : (
            <span data-slot="node-summary" className="min-w-0 truncate text-xs text-fg-muted">
              <NodeSummary node={node} />
            </span>
          )}
          {collapsible && (
            <button
              type="button"
              data-slot="collapse-toggle"
              aria-expanded={!collapsed}
              aria-label={`${collapsed ? 'Expand' : 'Collapse'} ${node.id}`}
              onClick={(e) => {
                e.stopPropagation();
                toggle(node.id);
              }}
              className="nodrag nopan relative z-10 inline-flex shrink-0 items-center gap-0.5 rounded-sm px-1 text-2xs font-medium text-fg-muted hover:bg-hover hover:text-fg"
            >
              {collapsed ? (
                <ChevronRight className="size-3.5" aria-hidden="true" />
              ) : (
                <ChevronDown className="size-3.5" aria-hidden="true" />
              )}
              {collapsed ? `${hiddenCount} hidden` : null}
            </button>
          )}
        </div>
      </div>
      <Handle
        type="source"
        position={Position.Bottom}
        className="size-2! opacity-0!"
        isConnectable={false}
      />
    </div>
  );
}
