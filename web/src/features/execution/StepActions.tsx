// Inline per-step actions (M2): claim, checklist, outputs + report,
// release, approval decisions, and wait signals. Claim happens before
// the step read (D32/F60); checklist toggles are local first, saved one
// request at a time, and roll back on refusal (R53); F100 surfaces
// claims this user holds in another tab.
import { useEffect, useMemo, useRef, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { ApiError, type FieldError, type Schemas } from '../../api/client';
import { useMeta, type MeView } from '../../api/hooks';
import { EvidenceAttachment, type EvidenceUploadItem } from './evidence/EvidenceAttachment';
import {
  ApprovalPanel,
  ChecklistForm,
  OutputsForm,
  ReportBar,
  type JsonSchema,
  type ProblemError,
} from '../../ui/actions';
import { Button } from '../../ui/primitives/button';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';
import { Textarea } from '../../ui/primitives/textarea';
import {
  useChecklistQueue,
  useClaimStep,
  useDecide,
  useHumanReadyWork,
  useReleaseStep,
  useReportStep,
  useSignal,
  useStepRead,
  stepReadKey,
  type StepSectionData,
  type StepViewData,
} from './hooks';
import {
  friendlyError,
  hasVoted,
  isApprovalOpen,
  isClaimableByPerson,
  isEligibleToDecide,
  type Capabilities,
} from './gating';
import { claimStillHeld, tabClaims } from './session';
import {
  canTakeOver,
  strictAssigneeAllows,
  strictAssigneeLabel,
  useTakeOverStep,
} from './takeOver';
import { isUserId, shortId, stepHeadingId } from '../../ui/execution';
import type { DirectoryNames } from '../context/names';

export interface StepActionsProps {
  executionId: string;
  section: StepSectionData;
  step: StepViewData | undefined;
  me: MeView | undefined;
  capabilities: Capabilities;
  /** When the document these props come from was read (epoch ms). */
  documentAt: number;
  /** Directory names for user and team ids (walkthrough #1). */
  names?: DirectoryNames;
  /**
   * Whether this tab's claimed-step form (with its checklist) is showing,
   * so the section can hide its read-only copy of the checklist (#6).
   */
  onFormShownChange?: (shown: boolean) => void;
  /** The run is paused: no claim or take-over is offered (the server refuses). */
  runPaused?: boolean;
  /**
   * Whether to say "Assigned to … (strict)" in place of a claim this person
   * may not make. False where the caller already shows the strict assignee
   * (UI-3's side panel).
   */
  strictNote?: boolean;
  /** The run's project (id or slug), for runner transcript links. */
  project?: string;
}

const NO_NAMES: DirectoryNames = { nameOf: shortId, teamName: (reference) => reference };

/**
 * A decided approval's outcome from its step outputs (`decision`,
 * `decided_by`). An agent's `decided_by` is its principal (`runner:…`), so
 * the agent's name comes from its recorded decision (R1-D2).
 */
function approvalOutcome(
  outputs: Record<string, unknown> | undefined,
  nameOf: DirectoryNames['nameOf'],
  decisions: Schemas['DecisionRecord'][] = [],
): { option: string; by?: string } | null {
  const option = outputs?.decision;
  if (typeof option !== 'string' || option.length === 0) return null;
  const by = outputs?.decided_by;
  if (typeof by !== 'string' || by.length === 0) return { option };
  if (isUserId(by)) return { option, by: nameOf(by) };
  const agent = decisions.find(
    (decision) =>
      decision.by === 'agent' &&
      decision.option === option &&
      (decision.principal == null || decision.principal === by),
  );
  if (agent?.by === 'agent') return { option, by: `agent ${agent.agent_name}` };
  return { option, by: by === 'system' ? 'System policy' : by };
}

/** Refusals meaning this tab's claim is over: drop it and its form. */
const CLAIM_GONE = new Set([
  'claim.conflict',
  'claim.stale_generation',
  'claim.not_found',
  'step.completed',
]);

/**
 * 422 field errors for the outputs form: the engine names output fields
 * `outputs.<name>` (with `.sub` or `[i]` below them); the form marks the
 * top-level field.
 */
function outputFieldErrors(errors: FieldError[]): ProblemError[] {
  return errors.flatMap((error) => {
    const match = /^outputs\.([A-Za-z_][A-Za-z0-9_]*)/.exec(error.path);
    return match ? [{ path: `outputs.${match[1]}`, message: error.message, code: error.code }] : [];
  });
}

function errorText(error: unknown): string {
  return error instanceof ApiError
    ? friendlyError(error.code)
    : 'That action could not be completed.';
}

export function StepActions({
  executionId,
  section,
  step,
  me,
  capabilities,
  documentAt,
  names = NO_NAMES,
  onFormShownChange,
  runPaused = false,
  strictNote = true,
  project,
}: StepActionsProps) {
  const stepRunId = section.step_run_id;
  const [storedClaim, setMyClaim] = useState(() => tabClaims.get(stepRunId));
  // U5: every document read reconciles the remembered claim. One the
  // document no longer shows as this tab's (released or re-claimed in
  // another tab, reported, cancelled, overridden) is not ours any more.
  const takenAt = tabClaims.takenAt(stepRunId) ?? 0;
  const held = storedClaim
    ? claimStillHeld(storedClaim, takenAt, section, step, documentAt)
    : false;
  const myClaim = held ? storedClaim : undefined;
  const [actionError, setActionError] = useState<string | null>(null);
  const [verdict, setVerdict] = useState<Schemas['Verdict'] | ''>('');
  const [signalName, setSignalName] = useState('');
  const [summary, setSummary] = useState('');
  // U11: after claim, report or release the pressed control unmounts; once
  // that commit lands, focus goes to the step's heading so a keyboard user
  // keeps their place. Never taken from a field the person moved to.
  const [focusRequest, setFocusRequest] = useState(0);
  const container = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (focusRequest === 0) return;
    const active = document.activeElement;
    const lost = !active || active === document.body || !active.isConnected;
    if (!lost && !container.current?.contains(active)) return;
    document.getElementById(stepHeadingId(stepRunId))?.focus({ preventScroll: true });
  }, [focusRequest, stepRunId]);
  const focusSectionHeading = () => setFocusRequest((n) => n + 1);
  const [fieldErrors, setFieldErrors] = useState<FieldError[]>([]);
  const [claimTick, setClaimTick] = useState(0);

  const meta = useMeta();
  const uploadsEnabled = Boolean(meta.data?.capabilities?.uploads);
  const uploadLimits = meta.data?.limits?.uploads;
  const maxRequestBytes = meta.data?.limits?.max_request_bytes;
  const [attachedEvidence, setAttachedEvidence] = useState<EvidenceUploadItem[]>([]);

  const claimStep = useClaimStep(executionId);
  const takeOver = useTakeOverStep(executionId);
  const stepRead = useStepRead(stepRunId, Boolean(myClaim));
  const checklist = useChecklistQueue(executionId, stepRunId);
  const reportStep = useReportStep(executionId);
  const releaseStep = useReleaseStep(executionId);
  const decide = useDecide(executionId);
  const signal = useSignal(executionId);
  const humanReady = useHumanReadyWork(executionId, capabilities.canAct);

  // Checklist ticks derive from the read + document until this tab ticks
  // one; from then on its own list (kept by the queue) is what shows and
  // what is sent, so a stale document never clears a tick (R53).
  const derivedTicks = useMemo<Schemas['ChecklistTick'][] | null>(() => {
    if (!stepRead.data) return null;
    // By position: two items may share a title (U18). The server keeps the
    // rendered list's order, so the same index is the same item.
    const saved = step?.checklist ?? [];
    return stepRead.data.checklist.map((title, index) => ({
      title,
      completed: saved[index]?.title === title ? (saved[index]?.completed ?? false) : false,
    }));
  }, [stepRead.data, step?.checklist]);
  const ticks = checklist.ticks ?? derivedTicks;

  const queryClient = useQueryClient();
  /** Forget the claim outside React state: the store, local ticks and the cached read. */
  const forgetClaim = () => {
    tabClaims.clear(stepRunId);
    checklist.reset();
    queryClient.removeQueries({ queryKey: stepReadKey(stepRunId) });
  };
  const dropClaim = () => {
    forgetClaim();
    setAttachedEvidence([]);
    setMyClaim(undefined);
  };
  const lostClaim = Boolean(storedClaim) && !held;
  useEffect(() => {
    if (lostClaim) forgetClaim();
    // forgetClaim only touches stores and the checklist queue's reset.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lostClaim]);

  const showError = (error: unknown) => {
    setActionError(errorText(error));
    if (error instanceof ApiError && CLAIM_GONE.has(error.code)) dropClaim();
  };

  const claimedByMeElsewhere =
    !myClaim &&
    Boolean(step?.claim) &&
    section.actor?.kind === 'human' &&
    section.actor.user_id === me?.user?.id &&
    (section.status === 'claimed' || step?.status === 'claimed');

  // Ticking stays open while a checklist save is in flight (the queue
  // serializes them); everything else waits for the saves to land.
  const uploadsInFlight = attachedEvidence.some(
    (item) =>
      item.status === 'requesting_slot' ||
      item.status === 'uploading' ||
      item.status === 'finalizing',
  );
  const otherBusy =
    claimStep.isPending ||
    takeOver.isPending ||
    reportStep.isPending ||
    releaseStep.isPending ||
    decide.isPending ||
    signal.isPending;
  const busy = otherBusy || checklist.saving || uploadsInFlight;

  async function onClaim() {
    setActionError(null);
    try {
      const claim = await claimStep.mutateAsync({ stepRunId });
      checklist.reset();
      setAttachedEvidence([]);
      setMyClaim(claim);
      setClaimTick((n) => n + 1);
      focusSectionHeading();
    } catch (error) {
      showError(error);
    }
  }

  async function onTakeOver() {
    setActionError(null);
    try {
      const claim = await takeOver.mutateAsync({ stepRunId });
      checklist.reset();
      setAttachedEvidence([]);
      setMyClaim(claim);
      setClaimTick((n) => n + 1);
      focusSectionHeading();
    } catch (error) {
      showError(error);
    }
  }

  async function onRelease(claim: Schemas['ClaimRef']) {
    setActionError(null);
    try {
      await releaseStep.mutateAsync({ stepRunId, claim });
      checklist.reset();
      setAttachedEvidence([]);
      setMyClaim(undefined);
      focusSectionHeading();
    } catch (error) {
      showError(error);
    }
  }

  function onToggleChecklist(index: number, completed: boolean) {
    if (!myClaim || !ticks) return;
    setActionError(null);
    checklist.toggle(ticks, index, completed, myClaim, showError);
  }

  /** One report: status, summary, outputs and verdict in one POST (U8). */
  async function report(
    status: Schemas['ReportStatus'],
    reportSummary?: string,
    outputs?: Record<string, unknown>,
  ) {
    if (!myClaim) return;
    setActionError(null);
    setFieldErrors([]);
    const readyEvidence = attachedEvidence
      .filter((item) => item.status === 'ready' && item.evidence?.id)
      .map((item) => item.evidence!.id);
    try {
      await reportStep.mutateAsync({
        stepRunId,
        claim: myClaim,
        status,
        summary: reportSummary?.trim() || undefined,
        outputs,
        verdict: verdict || undefined,
        evidence: readyEvidence,
      });
      checklist.reset();
      setSummary('');
      setAttachedEvidence([]);
      setMyClaim(undefined);
      focusSectionHeading();
    } catch (error) {
      if (error instanceof ApiError && error.status === 422) setFieldErrors(error.errors);
      showError(error);
    }
  }

  const formShown = Boolean(myClaim && stepRead.data);
  useEffect(() => {
    onFormShownChange?.(formShown);
  }, [formShown, onFormShownChange]);

  const personWork =
    capabilities.canAct &&
    !runPaused &&
    !myClaim &&
    !claimedByMeElsewhere &&
    isClaimableByPerson(section, humanReady.data);
  // LANG7: a strict assignee is the only person who may claim; anyone else
  // sees who it is assigned to instead of a claim the server would refuse.
  const strictAllows = strictAssigneeAllows(section, me);
  const canClaim = personWork && strictAllows;
  const assignedElsewhere =
    strictNote && personWork && !strictAllows ? strictAssigneeLabel(section, names) : null;

  // Take-over waits for the human brief: a handed-off agent step is
  // claimed, not taken over.
  const offerTakeOver =
    !myClaim &&
    !claimedByMeElsewhere &&
    humanReady.data !== undefined &&
    canTakeOver(section, me, capabilities, { paused: runPaused, canClaim: personWork });

  const approval = section.approval;
  const eligible = isEligibleToDecide(approval, me);
  const showSignal = capabilities.canAct && section.kind === 'wait' && section.status === 'waiting';
  const outputSchema = (stepRead.data?.output_schema ?? null) as JsonSchema | null;
  const hasOutputFields = Boolean(
    outputSchema?.properties && Object.keys(outputSchema.properties).length > 0,
  );
  const outputErrors = outputFieldErrors(fieldErrors);
  // Errors the outputs form cannot show (summary, verdict, evidence...).
  const otherFieldErrors = fieldErrors.filter((error) => !/^outputs\./.test(error.path));

  if (
    !canClaim &&
    !offerTakeOver &&
    !assignedElsewhere &&
    !myClaim &&
    !claimedByMeElsewhere &&
    !approval &&
    !showSignal &&
    !actionError
  ) {
    return null;
  }

  return (
    <div
      ref={container}
      className="grid gap-3 border-t border-border pt-3"
      data-testid={`actions-${stepRunId}`}
    >
      {actionError ? (
        <div role="alert" className="text-base">
          <p>{actionError}</p>
          {otherFieldErrors.length > 0 ? (
            <ul className="list-disc pl-5">
              {otherFieldErrors.map((error) => (
                <li key={`${error.path}:${error.code}`}>
                  <span className="font-mono">{error.path}</span>: {error.message}
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : null}

      {claimedByMeElsewhere && step?.claim ? (
        <div className="flex flex-wrap items-center gap-2">
          <p>This step is claimed by you elsewhere (another tab or session).</p>
          <Button
            variant="secondary"
            disabled={busy}
            onClick={() => void onRelease(step.claim as Schemas['ClaimRef'])}
          >
            Release it here
          </Button>
        </div>
      ) : null}

      {canClaim ? (
        <div>
          <Button disabled={busy} onClick={() => void onClaim()}>
            {claimStep.isPending ? 'Claiming…' : 'Claim this step'}
          </Button>
        </div>
      ) : null}

      {assignedElsewhere ? (
        <p className="text-sm text-fg-muted">Assigned to {assignedElsewhere} (strict)</p>
      ) : null}

      {offerTakeOver ? (
        <div className="flex flex-wrap items-center gap-2">
          <Button variant="secondary" disabled={busy} onClick={() => void onTakeOver()}>
            {takeOver.isPending ? 'Taking over…' : 'Take over'}
          </Button>
          <p className="text-sm text-fg-muted">
            This step is for an agent; you can do it yourself.
          </p>
        </div>
      ) : null}

      {myClaim ? (
        <div className="grid gap-3">
          {stepRead.isLoading ? <p role="status">Loading step instructions…</p> : null}
          {stepRead.isError ? <p role="alert">{errorText(stepRead.error)}</p> : null}
          {stepRead.data ? (
            <>
              <div className="whitespace-pre-wrap">{stepRead.data.instructions}</div>
              {ticks && ticks.length > 0 ? (
                <ChecklistForm items={ticks} disabled={otherBusy} onToggle={onToggleChecklist} />
              ) : null}
              <div className="grid gap-1">
                <Label htmlFor={`verdict-${stepRunId}`}>
                  Check verdict, if this step is a check
                </Label>
                <select
                  id={`verdict-${stepRunId}`}
                  value={verdict}
                  disabled={busy}
                  onChange={(event) => setVerdict(event.target.value as Schemas['Verdict'] | '')}
                >
                  <option value="">No verdict</option>
                  <option value="pass">Verdict pass</option>
                  <option value="warn">Verdict warn</option>
                  <option value="fail">Verdict fail</option>
                </select>
              </div>
              <div className="grid gap-1">
                <Label htmlFor={`summary-${stepRunId}`}>Summary (optional)</Label>
                <Textarea
                  id={`summary-${stepRunId}`}
                  rows={2}
                  value={summary}
                  disabled={busy}
                  onChange={(event) => setSummary(event.target.value)}
                />
              </div>
              <EvidenceAttachment
                stepRunId={stepRunId}
                uploadsEnabled={uploadsEnabled}
                limits={uploadLimits}
                maxRequestBytes={maxRequestBytes}
                items={attachedEvidence}
                onItemsChange={setAttachedEvidence}
                disabled={busy}
              />
              {hasOutputFields && outputSchema ? (
                // Done lives on the outputs form, so the values, the
                // summary and the verdict always go in one report.
                <OutputsForm
                  key={claimTick}
                  schema={outputSchema}
                  disabled={busy}
                  errors={outputErrors}
                  submitLabel="Done"
                  onSubmit={(values) => void report('succeeded', summary, values)}
                />
              ) : null}
              <ReportBar
                disabled={busy}
                allowSkip
                showDone={!hasOutputFields}
                summary={summary}
                onReport={(payload) => {
                  const status =
                    payload.action === 'done'
                      ? 'succeeded'
                      : payload.action === 'skip'
                        ? 'skipped'
                        : payload.action;
                  void report(status, payload.summary);
                }}
              />
              <div>
                <Button variant="secondary" disabled={busy} onClick={() => void onRelease(myClaim)}>
                  Release claim
                </Button>
              </div>
            </>
          ) : null}
        </div>
      ) : null}

      {approval ? (
        <div className="grid gap-2">
          <ApprovalPanel
            approval={approval}
            nameOf={names.nameOf}
            teamName={names.teamName}
            outcome={approvalOutcome(section.outputs, names.nameOf, approval.decisions)}
            agent={
              step?.runner
                ? {
                    name: step.runner.agent_name,
                    readyAtMs: step.runner.decision_ready_at_ms,
                  }
                : undefined
            }
            transcriptHref={
              project
                ? `/p/${encodeURIComponent(project)}/agents/transcripts/${encodeURIComponent(stepRunId)}`
                : undefined
            }
            now={documentAt}
            disabled={busy || !eligible}
            onDecide={(option, comment) => {
              setActionError(null);
              decide.mutate({ stepRunId, option, comment }, { onError: showError });
            }}
            // In the panel, next to the buttons it explains (walkthrough #7).
            // The panel itself says when the approval is closed.
            notice={
              isApprovalOpen(approval) && hasVoted(approval, me)
                ? 'You have voted on this approval.'
                : isApprovalOpen(approval) && !eligible
                  ? 'You are not eligible to decide this approval.'
                  : null
            }
          />
        </div>
      ) : null}

      {showSignal ? (
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            setActionError(null);
            signal.mutate({ name: signalName.trim() }, { onError: showError });
          }}
        >
          <div className="grid gap-1">
            <Label htmlFor={`signal-${stepRunId}`}>Signal name</Label>
            <Input
              id={`signal-${stepRunId}`}
              value={signalName}
              disabled={busy}
              onChange={(event) => setSignalName(event.target.value)}
            />
          </div>
          <Button
            type="submit"
            variant="secondary"
            disabled={busy || signalName.trim().length === 0}
          >
            Send signal
          </Button>
        </form>
      ) : null}
    </div>
  );
}
