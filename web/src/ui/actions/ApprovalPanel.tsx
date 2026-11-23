import { useId, useState, type ReactNode } from 'react';
import { Bot } from 'lucide-react';
import type { components } from '../../api/schema';
import { Button } from '../primitives/button';
import { Input } from '../primitives/input';
import { Label } from '../primitives/label';
import { cn } from '../primitives/cn';
import { ActorBadge } from '../status/ActorBadge';
import { shortId, type NameOf } from '../execution/names';
import { formatRelativeTime } from '../../lib/format';
import { ConfirmDialog } from './ConfirmDialog';

export type ApprovalView = components['schemas']['ApprovalView'];
export type ApprovalOption = components['schemas']['ApprovalOption'];
export type DecisionRecord = components['schemas']['DecisionRecord'];
export type RecipientRef = components['schemas']['RecipientRef'];
export type DecisionLeaseView = components['schemas']['DecisionLeaseView'];
export type ApprovalAgentNote = components['schemas']['ApprovalAgentNote'];

/**
 * R1-D2: the runner agent an `allow_agents` approval names (`agent:`),
 * and when its decision work becomes ready (`agent.after` past opening).
 */
export interface ApprovalAgent {
  name: string;
  /** Epoch ms; absent until the approval is requested. */
  readyAtMs?: number | null;
}

/**
 * What a runner agent is doing on an open approval, in words: reviewing
 * (a decision lease is held), abstained, or when it may decide. Null when
 * the approval names no runner agent, or once it is closed.
 */
export function agentReviewText(
  approval: Pick<ApprovalView, 'status' | 'decision_lease' | 'agent_notes'>,
  agent: ApprovalAgent | undefined,
  now: number,
): string | null {
  if (!isApprovalOpen(approval)) return null;
  const lease = approval.decision_lease;
  if (lease) return `Agent ${lease.agent_name} is reviewing`;
  const notes = approval.agent_notes ?? [];
  const last = notes[notes.length - 1];
  if (last) return `Agent ${last.agent_name} abstained`;
  if (!agent) return null;
  if (agent.readyAtMs != null && agent.readyAtMs > now) {
    return `Agent ${agent.name} may decide ${formatRelativeTime(agent.readyAtMs, now)} if no one has`;
  }
  return agent.readyAtMs != null
    ? `Agent ${agent.name} may decide now`
    : `Agent ${agent.name} may decide`;
}

export interface ExtendedApprovalOption extends ApprovalOption {
  destructive?: boolean;
}

export interface ApprovalPanelProps {
  approval: ApprovalView & {
    options: ExtendedApprovalOption[];
  };
  onDecide?: (optionId: string, comment?: string) => void;
  disabled?: boolean;
  /** Resolves user ids to names (the org directory); defaults to a short id. */
  nameOf?: NameOf;
  /** Resolves a team reference (id or slug) to its slug. */
  teamName?: (reference: string) => string;
  /**
   * The step's recorded outcome once decided (its `decision` and
   * `decided_by` outputs, `by` already a display name). Without it the
   * panel reads the outcome from the recorded decisions.
   */
  outcome?: { option: string; by?: string } | null;
  /** Why this person cannot vote (not eligible, already voted): shown in the panel. */
  notice?: ReactNode;
  /** R1-D2: the runner agent this approval lets decide (`agent:`), if any. */
  agent?: ApprovalAgent;
  /**
   * R1-D2: the runner transcript of this approval's decision runs (keyed
   * by the approval's step run). Linked from agent decisions.
   */
  transcriptHref?: string;
  /** Clock for "may decide in 15m" (epoch ms); defaults to mount time. */
  now?: number;
  className?: string;
}

/** An approval takes votes only while its step is ready or waiting. */
export function isApprovalOpen(approval: Pick<ApprovalView, 'status'>): boolean {
  return approval.status === 'ready' || approval.status === 'waiting';
}

function formatRecipients(
  recipients: RecipientRef[],
  nameOf: NameOf,
  teamName: (reference: string) => string,
): string {
  if (!recipients || recipients.length === 0) return 'None';
  return recipients
    .map((r) => (r.kind === 'team' ? `@${teamName(r.reference)}` : nameOf(r.reference)))
    .join(', ');
}

const sameTeam = (reference: string) => reference;

function decisionAuthor(decision: DecisionRecord, nameOf: NameOf): string {
  if (decision.by === 'human') return nameOf(decision.user_id);
  if (decision.by === 'agent') return `agent ${decision.agent_name}`;
  return 'System policy';
}

/**
 * One line for a closed approval (walkthrough #5): "Decided: keep by Bob",
 * or how it closed when nobody decided. From the step's outcome when the
 * page has it, else from the votes for the last option chosen.
 */
export function closedApprovalText(
  approval: Pick<ApprovalView, 'status' | 'decisions' | 'options'>,
  nameOf: NameOf = shortId,
  outcome?: { option: string; by?: string } | null,
): string {
  const titleOf = (option: string) =>
    approval.options.find((candidate) => candidate.id === option)?.title ?? option;
  if (outcome?.option) {
    return `Decided: ${titleOf(outcome.option)}${outcome.by ? ` by ${outcome.by}` : ''}`;
  }
  const last = approval.decisions[approval.decisions.length - 1];
  const status = approval.status.replace(/_/g, ' ');
  if (!last) {
    return approval.status === 'succeeded'
      ? `Closed (${status}).`
      : `Closed without a decision (${status}).`;
  }
  const authors = [
    ...new Set(
      approval.decisions
        .filter((decision) => decision.option === last.option)
        .map((decision) => decisionAuthor(decision, nameOf)),
    ),
  ];
  return `Decided: ${titleOf(last.option)} by ${authors.join(', ')}`;
}

export function ApprovalPanel({
  approval,
  onDecide,
  disabled = false,
  nameOf = shortId,
  teamName = sameTeam,
  outcome,
  notice,
  agent,
  transcriptHref,
  now: nowProp,
  className,
}: ApprovalPanelProps) {
  const [comment, setComment] = useState('');
  const [mountedAt] = useState(() => Date.now());
  const now = nowProp ?? mountedAt;
  // Every vote is confirmed: a recorded vote cannot be changed (U16).
  const [pendingOption, setPendingOption] = useState<ExtendedApprovalOption | null>(null);
  const commentId = useId();
  const open = isApprovalOpen(approval);

  const isDestructive = (option: ExtendedApprovalOption): boolean => {
    return (
      Boolean(option.destructive) ||
      option.id.toLowerCase().includes('rollback') ||
      option.id.toLowerCase().includes('abort') ||
      option.title.toLowerCase().includes('roll back')
    );
  };

  const handleConfirm = () => {
    if (pendingOption) {
      onDecide?.(pendingOption.id, comment.trim() || undefined);
      setPendingOption(null);
    }
  };
  const pendingDestructive = pendingOption ? isDestructive(pendingOption) : false;

  const eligibleText = formatRecipients(approval.eligible_recipients, nameOf, teamName);
  const escalationText = formatRecipients(approval.escalation_recipients, nameOf, teamName);
  const hasDecisions = approval.decisions && approval.decisions.length > 0;

  return (
    <div
      data-slot="approval-panel"
      className={cn(
        'flex flex-col gap-4 p-4 border border-border rounded-lg bg-surface',
        className,
      )}
    >
      {/* Prompt while open; once closed, the outcome and no call to action. */}
      <div>
        <h4 className="text-xs font-semibold uppercase tracking-wider text-fg-muted">
          {open ? 'Approval required' : 'Approval'}
        </h4>
        <p className="mt-1 text-sm font-medium text-fg">
          {open ? approval.prompt : closedApprovalText(approval, nameOf, outcome)}
        </p>
      </div>

      {/* Eligibility line */}
      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1 text-2xs text-fg-muted font-mono">
        <span>Eligible: {eligibleText}</span>
        <span>·</span>
        <span>Quorum: {approval.quorum}</span>
        {approval.escalation_recipients?.length > 0 && (
          <>
            <span>·</span>
            <span>Escalation: {escalationText}</span>
          </>
        )}
      </div>

      <AgentReview approval={approval} agent={agent} now={now} open={open} />

      {open ? (
        <>
          {/* Comment input */}
          <div className="grid gap-1">
            <Label htmlFor={commentId} className="text-2xs font-medium text-fg-muted">
              Decision comment (optional)
            </Label>
            <Input
              id={commentId}
              type="text"
              placeholder="Add reason or context..."
              value={comment}
              disabled={disabled}
              onChange={(e) => setComment(e.target.value)}
            />
          </div>

          {/* Options actions */}
          <div className="flex flex-wrap gap-2 pt-1">
            {approval.options.map((option) => {
              const destructive = isDestructive(option);
              return (
                <Button
                  key={option.id}
                  variant={destructive ? 'destructive' : 'primary'}
                  size="default"
                  disabled={disabled}
                  onClick={() => setPendingOption(option)}
                >
                  {option.title}
                </Button>
              );
            })}
          </div>
        </>
      ) : null}

      {notice ? <p className="text-sm text-fg-muted">{notice}</p> : null}

      {/* Every vote is confirmed */}
      <ConfirmDialog
        open={pendingOption !== null}
        onOpenChange={(isOpen) => {
          if (!isOpen) setPendingOption(null);
        }}
        title={`Confirm: ${pendingOption?.title ?? 'Action'}`}
        description={
          pendingDestructive
            ? 'This is a destructive action and cannot be undone.'
            : 'Your vote is recorded and cannot be changed.'
        }
        confirmLabel={pendingOption?.title ?? 'Confirm'}
        destructive={pendingDestructive}
        onConfirm={handleConfirm}
      />

      {/* Recorded decisions */}
      {hasDecisions && (
        <div className="mt-2 pt-3 border-t border-border flex flex-col gap-2">
          <span className="text-2xs font-semibold uppercase tracking-wider text-fg-muted">
            Recorded Decisions ({approval.decisions.length})
          </span>
          <div className="flex flex-col gap-2">
            {approval.decisions.map((decision, idx) => {
              if (decision.by === 'human') {
                return (
                  <div key={idx} className="flex flex-wrap items-center gap-2 text-xs">
                    <ActorBadge
                      actor={{ kind: 'human', user_id: decision.user_id }}
                      name={nameOf(decision.user_id)}
                    />
                    <span className="font-semibold text-fg">chose {decision.option}</span>
                    {decision.comment && (
                      <span className="text-fg-muted italic">"{decision.comment}"</span>
                    )}
                  </div>
                );
              }

              if (decision.by === 'agent') {
                return (
                  <AgentDecision key={idx} decision={decision} transcriptHref={transcriptHref} />
                );
              }

              // System decision
              return (
                <div key={idx} className="flex flex-wrap items-center gap-2 text-xs">
                  <ActorBadge actor={{ kind: 'system' }} />
                  <span className="font-semibold text-fg">chose {decision.option}</span>
                  <span className="text-fg-muted font-mono text-2xs">({decision.policy})</span>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

type AgentDecisionRecord = Extract<DecisionRecord, { by: 'agent' }>;

/**
 * R1-D2: an agent's decision with its provenance: who (agent and model),
 * the option, its reason, the `[call N]` evidence it cited, and the
 * transcript that holds those calls. Never a human signature.
 */
function AgentDecision({
  decision,
  transcriptHref,
}: {
  decision: AgentDecisionRecord;
  transcriptHref?: string;
}) {
  const evidence = decision.evidence ?? [];
  // Runner decisions carry the runner's principal; their transcript is the
  // runner's. External agents' decisions have no runner transcript.
  const fromRunner = Boolean(decision.principal?.startsWith('runner:'));
  const leaseHref = fromRunner && transcriptHref && decision.session_id
    ? `${transcriptHref}?claim_id=${encodeURIComponent(decision.session_id)}`
    : undefined;
  return (
    <div data-slot="agent-decision" className="flex flex-col gap-1 text-xs">
      <div className="flex flex-wrap items-center gap-2">
        <Bot className="size-3.5 shrink-0 text-fg-muted" aria-hidden="true" />
        <span className="text-fg">
          Decided by agent <span className="font-mono">{decision.agent_name}</span>
          {decision.model ? <> ({decision.model})</> : null}
        </span>
        <span className="font-semibold text-fg">chose {decision.option}</span>
      </div>
      {decision.comment && <span className="text-fg-muted italic">"{decision.comment}"</span>}
      {evidence.length > 0 || leaseHref ? (
        <div className="flex flex-wrap items-center gap-2 text-2xs text-fg-muted">
          {evidence.length > 0 ? (
            <span>
              {fromRunner ? 'Runner-reported evidence: ' : 'Evidence: '}<span className="font-mono">{evidence.join(', ')}</span>
            </span>
          ) : null}
          {leaseHref ? (
            <a className="text-accent underline" href={leaseHref}>
              Runner transcript
            </a>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/**
 * R1-D2: what the runner agent is doing on this approval: reviewing now
 * (people can still decide; the first decision wins), its abstentions,
 * or when it may decide.
 */
function AgentReview({
  approval,
  agent,
  now,
  open,
}: {
  approval: Pick<ApprovalView, 'status' | 'decision_lease' | 'agent_notes'>;
  agent?: ApprovalAgent;
  now: number;
  open: boolean;
}) {
  const notes = approval.agent_notes ?? [];
  const lease = open ? approval.decision_lease : null;
  if (!lease && notes.length === 0 && !(open && agent)) return null;
  const waiting =
    open && !lease && notes.length === 0 ? agentReviewText(approval, agent, now) : null;
  return (
    <div data-slot="agent-review" className="flex flex-col gap-1.5 text-xs">
      {lease ? (
        <p role="status" className="flex flex-wrap items-center gap-1.5 text-fg">
          <Bot className="size-3.5 shrink-0 text-fg-muted" aria-hidden="true" />
          <span>
            Agent <span className="font-mono">{lease.agent_name}</span> is reviewing.
          </span>
          <span className="text-fg-muted">You can still decide; the first decision wins.</span>
        </p>
      ) : null}
      {notes.map((note) => (
        <p key={`${note.lease_id}-${note.at_ms}`} className="flex flex-wrap items-center gap-1.5">
          <Bot className="size-3.5 shrink-0 text-fg-muted" aria-hidden="true" />
          <span className="text-fg">
            Agent <span className="font-mono">{note.agent_name}</span>
            {note.model ? <> ({note.model})</> : null} abstained:{' '}
            <span className="text-fg-muted italic">{note.reason}</span>
          </span>
        </p>
      ))}
      {open && notes.length > 0 && !lease ? (
        <p className="text-fg-muted">People decide this approval.</p>
      ) : null}
      {waiting ? (
        <p className="flex flex-wrap items-center gap-1.5 text-fg-muted">
          <Bot className="size-3.5 shrink-0" aria-hidden="true" />
          <span>{waiting}. People can decide first.</span>
        </p>
      ) : null}
    </div>
  );
}
