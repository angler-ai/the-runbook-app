// Role/eligibility gating for inline actions. These only decide what to
// offer; the server re-authorizes every mutation and its errors win.
import type { Schemas } from '../../api/client';
import type { MeView } from '../../api/hooks';
import { securityCodeMessage } from '../mfa/messages';

export interface Capabilities {
  canAct: boolean;
  /** Pause, resume and cancel (D27). */
  canControl: boolean;
  /** The role allows control but the token lacks executions:admin: say so. */
  controlNeedsScope: boolean;
  canRerun: boolean;
}

/** Project roles that may run executions (`role_allows(.., Run)` in runbook-core). */
const RUN_ROLES = new Set(['operator', 'editor', 'admin']);

/**
 * What to offer, from the token's scopes (/v1/me) and the person's role in
 * the execution's project: a role, null when not a member, undefined while
 * unknown. The server checks both (`effective_permission` is token AND
 * role), so a known role that cannot run hides actor actions; an unknown
 * one leaves them to the server, except pause/resume/cancel, which need a
 * known operator-or-above role plus executions:admin (D27, PRD §22).
 */
export function capabilities(me: MeView | undefined, projectRole?: string | null): Capabilities {
  const scopes = new Set(me?.credential.scopes ?? []);
  const roleRuns = projectRole === undefined ? undefined : RUN_ROLES.has(projectRole ?? '');
  return {
    canAct: scopes.has('executions:act') && roleRuns !== false,
    canControl: scopes.has('executions:admin') && roleRuns === true,
    controlNeedsScope: !scopes.has('executions:admin') && roleRuns === true,
    canRerun: scopes.has('executions:start') && roleRuns !== false,
  };
}

export const CONTROL_SCOPE_HINT =
  'Pause, resume and cancel need a token with the executions:admin scope. Sign in with one to control this run.';

const CONTROL_FRIENDLY: Record<string, string> = {
  'auth.scope_missing':
    'Pausing, resuming or cancelling a run needs a token with the executions:admin scope. Sign in with a token that has it.',
  'auth.forbidden':
    'Your role in this project does not allow that: pausing, resuming and cancelling need operator or above.',
};

/** Pause/resume/cancel refusals, in words about those controls. */
export function controlError(code: string): string {
  return CONTROL_FRIENDLY[code] ?? friendlyError(code);
}

/**
 * Whether to offer "Claim this step" to a person (R55): a ready task the
 * server lists as human work (`humanReady`, from the human resume brief:
 * actor human or any, or handed off to a person). Never an agent-only step,
 * and never an approval, which is decided in the decision panel. Unknown
 * until the brief loads, so nothing is offered before then.
 */
export function isClaimableByPerson(
  section: Schemas['StepSection'],
  humanReady: ReadonlySet<string> | undefined,
): boolean {
  if (section.status !== 'ready' || section.approval) return false;
  if (section.kind != null && section.kind !== 'task') return false;
  return humanReady?.has(section.step_run_id) ?? false;
}

type RecipientRef = Schemas['RecipientRef'];

function recipientMatches(ref: RecipientRef, me: MeView): boolean {
  if (ref.kind === 'user') return me.user?.id === ref.reference;
  // Team references arrive as the team id or its slug; /me carries both.
  return me.teams.some((team) => team.id === ref.reference || team.slug === ref.reference);
}

/** An approval takes votes only while its step is ready or waiting. */
export function isApprovalOpen(approval: Pick<Schemas['ApprovalView'], 'status'>): boolean {
  return approval.status === 'ready' || approval.status === 'waiting';
}

export function hasVoted(
  approval: Schemas['ApprovalView'] | null | undefined,
  me: MeView | undefined,
): boolean {
  return Boolean(
    approval &&
    me?.user &&
    approval.decisions.some(
      (decision) => decision.by === 'human' && decision.user_id === me.user?.id,
    ),
  );
}

export function isEligibleToDecide(
  approval: Schemas['ApprovalView'] | null | undefined,
  me: MeView | undefined,
): boolean {
  if (!approval || !me?.user) return false;
  if (!isApprovalOpen(approval)) return false;
  if (!me.credential.scopes.includes('executions:act')) return false;
  if (hasVoted(approval, me)) return false;
  if (approval.blocked_reason === 'approval.review_missing_report') return false;
  if (approval.excluded_completers?.includes(me.user.id)) return false;
  const pool = approval.escalated
    ? [...approval.eligible_recipients, ...approval.escalation_recipients]
    : approval.eligible_recipients;
  return pool.some((ref) => recipientMatches(ref, me));
}

const FRIENDLY: Record<string, string> = {
  'execution.closed': 'This run is already closed, so that change cannot be applied.',
  'execution.completed': 'This run has already completed.',
  'execution.paused': 'This run is paused.',
  'execution.queued': 'This run is still queued; only cancelling is available.',
  'execution.not_running': 'This run is not running.',
  'execution.conflict': 'The run changed underneath that action. The view has been refreshed.',
  'execution.history_expired': 'This run’s history has expired.',
  'claim.conflict': 'That step was just claimed by someone else. The view has been refreshed.',
  'claim.stale_generation': 'That claim is no longer current. The view has been refreshed.',
  'approval.review_missing_report':
    'Work in this approval’s review scope timed out. A project admin can supply the missing result.',
  'approval.excluded_completer':
    'You contributed to work this approval reviews, directly or through an agent or service token.',
  'approval.unknown_service_contributor':
    'A service token contributor could not be identified. A named approver without project admin rights must review.',
  'approval.not_eligible': 'You are not eligible to decide this approval.',
  'approval.decided': 'This approval has already been decided.',
  'approval.expired': 'This approval has expired.',
  'approval.deadline_exceeded': 'This approval has closed.',
  'approval.not_open': 'This approval has closed.',
  'approval.duplicate_vote': 'You have already voted on this approval.',
  'step.not_ready': 'That step is not ready for this action.',
  'step.not_claimable': 'That step cannot be claimed.',
  'claim.not_assignee':
    'Only this step’s assignee (or, once it escalates, its escalation) can claim it.',
  'claim.take_over_live_claim':
    'An agent is working on this step right now. A project admin can release its claim first.',
  'claim.take_over_not_allowed': 'This step cannot be taken over.',
  'step.completed': 'That step has already completed.',
  'step.invalid_outputs':
    'Some outputs do not match what this step declares. Check the marked fields.',
  'step.skip_reason_required': 'Skipping this step needs a reason in the summary.',
  'step.not_skippable': 'This step cannot be skipped.',
  'step.verdict_required': 'This check needs a verdict (pass, warn, or fail) with the report.',
  'auth.scope_missing': 'Your token does not have the scope for that action.',
  'request.idempotency_key_reused': 'That action was already submitted with different details.',
  'upload.type_unsupported': 'This file format is not supported as evidence.',
  'upload.image_invalid': 'The image file could not be decoded or is corrupt.',
  'upload.image_too_large': 'The image dimensions or file size exceed supported limits.',
  'upload.processed_too_large': 'The processed image exceeds the supported file size.',
  'upload.limit_reached':
    'The maximum number of uploads for this step or organization storage limit has been reached.',
  'upload.url_expired': 'The upload window expired. Please try uploading again.',
  'upload.url_invalid': 'The upload signature is invalid. Please try again.',
  'upload.not_pending': 'This upload slot is no longer pending.',
  'upload.not_uploaded': 'No file content was uploaded to the slot.',
  'upload.size_mismatch': 'The uploaded file size did not match the declared size.',
  'upload.type_mismatch': 'The uploaded file type did not match the declared type.',
  'upload.duplicate': 'A file with this name has already been uploaded to this step.',
  'upload.slot_expired': 'The upload slot expired. Please try uploading again.',
  'uploads.not_configured': 'Evidence uploads are not configured on this server.',
};

export function friendlyError(code: string): string {
  return (
    FRIENDLY[code] ??
    securityCodeMessage(code) ??
    `The server refused that action (${code}). The view has been refreshed.`
  );
}
