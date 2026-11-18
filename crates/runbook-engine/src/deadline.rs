//! Absolute deadlines and independent pause reasons, driven only by supplied time.
use crate::{Command, EngineError, ExecutionState, StepState};
use runbook_model::{ExecutionStatus, RecipientRef, StepRunId, StepStatus};
use runbook_spec::ir::{ApprovalTimeout, ExpiredPolicy, NodeKind};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PauseReason {
    Manual,
    ApprovalExpired(StepRunId),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationRequest {
    pub base_sequence: u64,
    pub step_run_id: StepRunId,
    pub deadline_at_ms: i64,
    pub targets: Vec<RecipientRef>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationSnapshot {
    pub request: EscalationRequest,
    pub recipients: Vec<RecipientRef>,
    pub directory_revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ApprovalNoticeKind {
    Escalated,
    Expired,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalNotice {
    pub step_run_id: StepRunId,
    pub kind: ApprovalNoticeKind,
    pub recipients: Vec<RecipientRef>,
    pub directory_revision: Option<u64>,
}
impl ApprovalNotice {
    pub fn event_type(&self) -> &'static str {
        match self.kind {
            ApprovalNoticeKind::Escalated => "approval.escalated",
            ApprovalNoticeKind::Expired => "approval.expired",
        }
    }
}
pub(crate) fn open(state: &ExecutionState) -> bool {
    matches!(
        state.status,
        ExecutionStatus::Running | ExecutionStatus::Paused
    )
}
pub(crate) fn add(at: i64, millis: u64) -> Result<i64, EngineError> {
    at.checked_add(i64::try_from(millis).map_err(|_| EngineError::new("deadline.overflow"))?)
        .ok_or(EngineError::new("deadline.overflow"))
}
pub fn authorize_at_deadline(state: &ExecutionState, at_ms: i64) -> Result<(), EngineError> {
    if state.phase == crate::ExecutionPhase::Cleanup && open(state) {
        let cleanup = state
            .cleanup
            .as_ref()
            .ok_or(EngineError::invariant("cleanup phase has no state"))?;
        if at_ms >= cleanup.deadline_at_ms {
            return Err(EngineError::new("execution.cleanup_deadline_exceeded"));
        }
        return Ok(());
    }
    if state.execution_deadline_at_ms.is_some_and(|at| at_ms >= at) {
        return Err(EngineError::new("execution.deadline_exceeded"));
    }
    Ok(())
}
pub(crate) fn sync_pause(state: &mut ExecutionState) {
    if open(state) {
        state.status = if state.pause_reasons.is_empty() {
            ExecutionStatus::Running
        } else {
            ExecutionStatus::Paused
        };
    }
}
pub(crate) fn manual_pause(state: &mut ExecutionState, paused: bool) -> Result<(), EngineError> {
    if !open(state) {
        return Err(EngineError::new("execution.not_running"));
    }
    if paused {
        state.pause_reasons.insert(PauseReason::Manual);
    } else {
        state.pause_reasons.remove(&PauseReason::Manual);
    }
    sync_pause(state);
    Ok(())
}
fn approval<'a>(
    state: &'a ExecutionState,
    step: &StepState,
) -> Result<&'a runbook_spec::ir::Approval, EngineError> {
    match &state
        .node(&step.step_id)
        .ok_or(EngineError::invariant("approval declaration is absent"))?
        .kind
    {
        NodeKind::Approval(a) => Ok(a),
        _ => Err(EngineError::new("step.not_approval")),
    }
}
fn approval_deadline(state: &ExecutionState, step: &StepState) -> Result<Option<i64>, EngineError> {
    let Some(record) = &step.approval else {
        return Ok(None);
    };
    if record.expired || record.decision.is_some() {
        return Ok(None);
    }
    let Some(first) = step.deadline_at_ms else {
        return Ok(None);
    };
    if record.escalation.is_some() {
        let a = approval(state, step)?;
        let delay = a
            .escalation_timeout
            .as_ref()
            .ok_or(EngineError::invariant("escalation has no duration"))?;
        Ok(Some(add(first, delay.millis)?))
    } else {
        Ok(Some(first))
    }
}
fn escalation_request(
    state: &ExecutionState,
    step: &StepState,
    deadline: i64,
) -> Result<EscalationRequest, EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::invariant("approval declaration is absent"))?;
    let targets = runbook_spec::resolve_approval_escalation(state.compiled(), node)
        .map_err(|_| EngineError::new("approval.no_escalation_target"))?;
    Ok(EscalationRequest {
        base_sequence: state.sequence,
        step_run_id: step
            .id
            .clone()
            .ok_or(EngineError::invariant("approval has no run ID"))?,
        deadline_at_ms: deadline,
        targets,
    })
}
/// Return the next due transition; call again after committing its event.
/// At equal deadlines the execution deadline wins.
pub fn advance_time(state: &ExecutionState, at_ms: i64) -> Result<Vec<Command>, EngineError> {
    if !open(state) {
        return Ok(vec![]);
    }
    if let Some(command) = crate::automatic::pending(state, at_ms)? {
        return Ok(vec![command]);
    }
    let mut notices = crate::handoff::pending_notifications(state);
    notices.extend(pending_notifications(state));
    if !notices.is_empty() {
        return Ok(notices);
    }
    if let Some(command) = crate::work_notifications::pending_approval(state)? {
        return Ok(vec![command]);
    }
    // Choose the due transition before resolving policy-dependent commands.
    // A later unsupported/invalid policy must not mask an earlier deadline.
    if state.phase == crate::ExecutionPhase::Cleanup {
        let cleanup = state
            .cleanup
            .as_ref()
            .ok_or(EngineError::invariant("cleanup phase has no state"))?;
        if cleanup.suppressed.is_none() && at_ms >= cleanup.deadline_at_ms {
            return Ok(vec![Command::SuppressCleanup {
                reason: if state
                    .limits
                    .hard_deadline_at_ms
                    .is_some_and(|hard| hard <= cleanup.deadline_at_ms)
                {
                    "limit.execution_duration"
                } else {
                    "cleanup.timeout"
                }
                .into(),
            }]);
        }
    }
    enum Due {
        Execution,
        Approval,
        Task,
        Timer(u32),
        Reminder,
        Escalation,
    }
    let mut candidates: Vec<(i64, u8, usize, Due)> = vec![];
    if let Some(at) = state
        .execution_deadline_at_ms
        .filter(|at| *at <= at_ms && state.phase == crate::ExecutionPhase::Ordinary)
    {
        candidates.push((at, 0, 0, Due::Execution));
    }
    for (index, step) in state.steps.iter().enumerate() {
        if crate::tree::terminal(step.status) {
            continue;
        }
        if step.approval.is_some() {
            if let Some(at) = approval_deadline(state, step)?.filter(|at| *at <= at_ms) {
                candidates.push((at, 1, index, Due::Approval));
            }
        } else if let Some(at) = step
            .deadline_at_ms
            .filter(|at| *at <= at_ms && !step.recovery_pending)
        {
            // A failed attempt awaiting its retry or handoff decision is not
            // timed out again by the deadline that ended it (F4).
            candidates.push((at, 2, index, Due::Task));
        }
        if step.status == StepStatus::Waiting {
            let timer = crate::wait::timer(step)
                .or_else(|| {
                    step.loop_state
                        .as_ref()
                        .and_then(|l| l.timer.as_ref())
                        .map(|t| (t.due_at_ms, t.generation))
                })
                .or_else(|| {
                    step.retry_timer
                        .as_ref()
                        .map(|t| (t.due_at_ms, t.generation))
                });
            if let Some((at, generation)) = timer.filter(|(at, _)| *at <= at_ms) {
                candidates.push((at, 3, index, Due::Timer(generation)));
            }
        }
        // LANG9: reminders and escalation for a step left open.
        if let Some(at) = crate::followup::reminder_due(state, step)?.filter(|at| *at <= at_ms) {
            candidates.push((at, 4, index, Due::Reminder));
        }
        if let Some(at) = crate::followup::escalation_due(state, step)?.filter(|at| *at <= at_ms) {
            candidates.push((at, 5, index, Due::Escalation));
        }
    }
    candidates.sort_by_key(|(at, priority, index, _)| (*at, *priority, *index));
    let Some((at, _, index, due)) = candidates.into_iter().next() else {
        return Ok(vec![]);
    };
    if matches!(due, Due::Execution) {
        return Ok(vec![Command::ExpireExecution { deadline_at_ms: at }]);
    }
    let step = &state.steps[index];
    let id = step
        .id
        .clone()
        .ok_or(EngineError::invariant("deadline has no run ID"))?;
    let command = match due {
        Due::Approval => {
            let record = step
                .approval
                .as_ref()
                .ok_or(EngineError::invariant("approval state disappeared"))?;
            if record.escalation.is_some() {
                Command::ExpireApproval {
                    step_run_id: id,
                    deadline_at_ms: at,
                }
            } else {
                match approval(state, step)?.on_timeout {
                    ApprovalTimeout::Escalate => Command::ResolveEscalation {
                        request: escalation_request(state, step, at)?,
                    },
                    ApprovalTimeout::Decide(_) => Command::ApplyAutomaticDecision {
                        policy: crate::TimeoutPolicy {
                            step_run_id: id,
                            deadline_at_ms: at,
                            at_ms,
                        },
                    },
                }
            }
        }
        Due::Task => Command::ExpireTask {
            step_run_id: id,
            deadline_at_ms: at,
        },
        Due::Timer(generation) => Command::FireWaitTimer {
            step_run_id: id,
            generation,
        },
        Due::Reminder => Command::RemindStep {
            step_run_id: id,
            due_at_ms: at,
        },
        Due::Escalation => Command::ResolveStepEscalation {
            request: crate::followup::escalation_request(state, step, at)?,
        },
        Due::Execution => return Err(EngineError::invariant("execution deadline was not handled")),
    };
    Ok(vec![command])
}

pub(crate) fn escalate(
    state: &mut ExecutionState,
    snapshot: &EscalationSnapshot,
    at_ms: i64,
) -> Result<usize, EngineError> {
    if !open(state) {
        return Err(EngineError::new("execution.not_running"));
    }
    let index = state.step_index(&snapshot.request.step_run_id)?;
    let step = &state.steps[index];
    let expected =
        approval_deadline(state, step)?.ok_or(EngineError::new("approval.not_escalating"))?;
    if step.status != StepStatus::Waiting
        || !matches!(approval(state, step)?.on_timeout, ApprovalTimeout::Escalate)
        || at_ms < expected
        || step
            .approval
            .as_ref()
            .is_some_and(|a| a.escalation.is_some())
        || snapshot.request != escalation_request(state, step, expected)?
        || snapshot.directory_revision == 0
        || snapshot
            .recipients
            .iter()
            .any(|r| !snapshot.request.targets.contains(r))
    {
        return Err(EngineError::new("approval.invalid_escalation"));
    }
    let step = &mut state.steps[index];
    step.routing_error = snapshot
        .recipients
        .is_empty()
        .then(|| "approval.no_escalation_target".into());
    let record = step
        .approval
        .as_mut()
        .ok_or(EngineError::invariant("approval state disappeared"))?;
    record.notices.push(ApprovalNotice {
        step_run_id: snapshot.request.step_run_id.clone(),
        kind: ApprovalNoticeKind::Escalated,
        recipients: snapshot.recipients.clone(),
        directory_revision: Some(snapshot.directory_revision),
    });
    record.escalation = Some(snapshot.clone());
    Ok(index)
}
pub(crate) fn expire_approval(
    state: &mut ExecutionState,
    id: &StepRunId,
    deadline: i64,
    at_ms: i64,
) -> Result<usize, EngineError> {
    if !open(state) {
        return Err(EngineError::new("execution.not_running"));
    }
    let auto_index = state.step_index(id)?;
    if matches!(
        approval(state, &state.steps[auto_index])?.on_timeout,
        ApprovalTimeout::Decide(_)
    ) {
        return crate::automatic::expire(
            state,
            &crate::TimeoutPolicy {
                step_run_id: id.clone(),
                deadline_at_ms: deadline,
                at_ms,
            },
        );
    }
    let index = state.step_index(id)?;
    let step = &state.steps[index];
    if step.status != StepStatus::Waiting
        || approval_deadline(state, step)? != Some(deadline)
        || at_ms < deadline
        || step
            .approval
            .as_ref()
            .is_none_or(|a| a.escalation.is_none())
    {
        return Err(EngineError::new("approval.not_expiring"));
    }
    let pause = approval(state, step)?.on_expired == ExpiredPolicy::Pause;
    let step = &mut state.steps[index];
    step.status = StepStatus::Expired;
    let record = step
        .approval
        .as_mut()
        .ok_or(EngineError::invariant("approval state disappeared"))?;
    let escalation = record
        .escalation
        .as_ref()
        .ok_or(EngineError::invariant("expiration has no escalation"))?;
    record.expired = true;
    record.notices.push(ApprovalNotice {
        step_run_id: id.clone(),
        kind: ApprovalNoticeKind::Expired,
        recipients: escalation.recipients.clone(),
        directory_revision: Some(escalation.directory_revision),
    });
    if pause {
        state
            .pause_reasons
            .insert(PauseReason::ApprovalExpired(id.clone()));
        sync_pause(state);
    }
    Ok(index)
}
pub(crate) fn pending_notifications(state: &ExecutionState) -> Vec<Command> {
    state
        .steps
        .iter()
        .filter_map(|s| s.approval.as_ref())
        .flat_map(|a| {
            a.notices
                .iter()
                .filter(|n| !a.queued_notices.contains(&n.kind))
        })
        .cloned()
        .map(|notice| Command::NotifyApproval { notice })
        .collect()
}
pub(crate) fn queue_notice(
    state: &mut ExecutionState,
    notice: &ApprovalNotice,
) -> Result<(), EngineError> {
    let index = state.step_index(&notice.step_run_id)?;
    let record = state.steps[index]
        .approval
        .as_mut()
        .ok_or(EngineError::new("step.not_approval"))?;
    let pending = record
        .notices
        .iter()
        .find(|pending| pending.kind == notice.kind)
        .ok_or(EngineError::new("notification.not_pending"))?;
    if !(pending == notice || expands(pending, notice))
        || !record.queued_notices.insert(notice.kind)
    {
        return Err(EngineError::new("notification.not_pending"));
    }
    Ok(())
}
/// The trusted runtime may queue a notice with its teams expanded to their
/// active users (as an approval request is), resolved against a directory
/// revision it records. Users may be added only when the pending notice
/// names a team; every other recipient must be one of its targets.
fn expands(pending: &ApprovalNotice, notice: &ApprovalNotice) -> bool {
    let has_team = pending
        .recipients
        .iter()
        .any(|r| matches!(r, RecipientRef::Team(_)));
    let distinct = notice
        .recipients
        .iter()
        .map(|r| match r {
            RecipientRef::User(id) => (0, id.as_str()),
            RecipientRef::Team(id) => (1, id.as_str()),
        })
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == notice.recipients.len();
    pending.step_run_id == notice.step_run_id
        && notice
            .directory_revision
            .is_some_and(|revision| revision > 0)
        && notice.recipients.len() <= 10_000
        && distinct
        && notice.recipients.iter().all(|r| {
            pending.recipients.contains(r) || (has_team && matches!(r, RecipientRef::User(_)))
        })
}

/// The earliest instant at which advance_time could produce a command:
/// execution and task deadlines, wait targets (`wait.for`/`wait.until`) or
/// wait timeouts, retry dues, loop timers and approval deadlines. Hosts
/// schedule one wakeup there.
pub fn next_wakeup(state: &ExecutionState) -> Result<Option<i64>, EngineError> {
    if !open(state) {
        return Ok(None);
    }
    let mut next: Option<i64> = None;
    let mut consider = |candidate: Option<i64>| {
        if let Some(at) = candidate {
            next = Some(next.map_or(at, |current: i64| current.min(at)));
        }
    };
    // Mirror `advance_time`: a deadline it would never act on (a finished
    // step's, or an escalated approval's first one) must not pin the wakeup
    // in the past, or later transitions are never re-armed.
    if state.phase == crate::ExecutionPhase::Ordinary {
        consider(state.execution_deadline_at_ms);
    }
    if state.phase == crate::ExecutionPhase::Cleanup
        && let Some(cleanup) = state.cleanup.as_ref().filter(|c| c.suppressed.is_none())
    {
        consider(Some(cleanup.deadline_at_ms));
    }
    for step in &state.steps {
        if crate::tree::terminal(step.status) {
            continue;
        }
        if step.approval.is_some() {
            consider(approval_deadline(state, step)?);
        } else if !step.recovery_pending {
            consider(step.deadline_at_ms);
        }
        if step.status == StepStatus::Waiting {
            // Mirror `advance_time`: a duration/absolute wait's target, or
            // its timeout, whichever comes first.
            consider(crate::wait::timer(step).map(|(at, _)| at));
            consider(step.retry_timer.as_ref().map(|retry| retry.due_at_ms));
            consider(
                step.loop_state
                    .as_ref()
                    .and_then(|state| state.timer.as_ref())
                    .map(|timer| timer.due_at_ms),
            );
        }
        // LANG9: mirror `advance_time`'s reminders and escalation.
        consider(crate::followup::reminder_due(state, step)?);
        consider(crate::followup::escalation_due(state, step)?);
    }
    Ok(next)
}
