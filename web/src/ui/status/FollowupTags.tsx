import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { shortId, type NameOf } from '../execution/names';
import { ArrowUpRight, BellRing } from 'lucide-react';

export type StepFollowupView = components['schemas']['StepFollowupView'];
export type TeamNameOf = (reference: string) => string;

export interface FollowupTagsProps {
  followup?: StepFollowupView | null;
  /** Resolves user ids to names; defaults to a short id. */
  nameOf?: NameOf;
  /** Resolves a team reference to its slug. */
  teamNameOf?: TeamNameOf;
  className?: string;
}

/** LANG9: what follow-ups have already happened on a step. */
export function FollowupTags({
  followup,
  nameOf = shortId,
  teamNameOf = (reference) => reference,
  className,
}: FollowupTagsProps) {
  if (!followup || (followup.reminders_sent === 0 && !followup.escalated)) return null;

  const recipients = (followup.escalated_to ?? []).map((recipient) =>
    recipient.kind === 'user'
      ? nameOf(recipient.reference)
      : `team ${teamNameOf(recipient.reference)}`,
  );
  const escalatedLabel =
    recipients.length > 0 ? `Escalated to ${recipients.join(', ')}` : 'Escalated';

  return (
    <>
      {followup.reminders_sent > 0 && (
        <span
          data-slot="reminded-tag"
          className={cn(
            'inline-flex items-center gap-1 rounded-sm bg-status-waiting-bg px-1.5 py-0.5 text-2xs font-medium text-status-waiting-fg select-none whitespace-nowrap',
            className,
          )}
        >
          <BellRing className="size-3 shrink-0" aria-hidden="true" />
          Reminded{followup.reminders_sent > 1 ? ` ×${followup.reminders_sent}` : ''}
        </span>
      )}
      {followup.escalated && (
        <span
          data-slot="escalated-tag"
          role="img"
          aria-label={escalatedLabel}
          title={escalatedLabel}
          className={cn(
            'inline-flex items-center gap-1 rounded-sm bg-status-paused-bg px-1.5 py-0.5 text-2xs font-medium text-status-paused-fg select-none whitespace-nowrap',
            className,
          )}
        >
          <ArrowUpRight className="size-3 shrink-0" aria-hidden="true" />
          Escalated
        </span>
      )}
    </>
  );
}
