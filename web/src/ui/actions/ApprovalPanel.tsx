import { useId, useState, type ReactNode } from 'react';
import type { components } from '../../api/schema';
import { Button } from '../primitives/button';
import { Input } from '../primitives/input';
import { Label } from '../primitives/label';
import { cn } from '../primitives/cn';
import { ActorBadge } from '../status/ActorBadge';
import { shortId, type NameOf } from '../execution/names';
import { ConfirmDialog } from './ConfirmDialog';

export type ApprovalView = components['schemas']['ApprovalView'];
export type ApprovalOption = components['schemas']['ApprovalOption'];
export type DecisionRecord = components['schemas']['DecisionRecord'];
export type RecipientRef = components['schemas']['RecipientRef'];

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
  if (decision.by === 'agent') return decision.agent_name;
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
  className,
}: ApprovalPanelProps) {
  const [comment, setComment] = useState('');
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
                  <div key={idx} className="flex flex-wrap items-center gap-2 text-xs">
                    <ActorBadge
                      actor={{
                        kind: 'agent',
                        name: decision.agent_name,
                        model: decision.model,
                        principal_id: decision.agent_name,
                        session_id: decision.session_id,
                      }}
                    />
                    <span className="font-semibold text-fg">chose {decision.option}</span>
                    {decision.comment && (
                      <span className="text-fg-muted italic">"{decision.comment}"</span>
                    )}
                  </div>
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
