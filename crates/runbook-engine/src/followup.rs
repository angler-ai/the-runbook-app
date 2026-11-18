//! LANG9: reminders and escalation for a step left open (ready, claimed or
//! waiting), counted from when it opened and driven only by supplied time.
//! A reminder is a notification and changes nothing else; an escalation adds
//! its recipients to the step's inbox (and, for a strict task, to who may
//! claim it).
use crate::{EngineError, EscalationRequest, EscalationSnapshot, ExecutionState, StepState};
use runbook_model::{ActorRef, ExecutionStatus, RecipientRef, StepRunId, StepStatus};
use runbook_spec::ir::{Assignee, Followup, NodeKind, Recipient};
use std::collections::BTreeMap;

/// Whether the step waits on a person or a signal: ready, claimed, or
/// waiting (an unresolved handoff included). A failed attempt waiting to be
/// retried is not open; the retry opens it again.
pub(crate) fn is_open(step: &StepState) -> bool {
    match step.status {
        StepStatus::Ready | StepStatus::Claimed => true,
        StepStatus::Waiting => {
            step.handoff.is_some() || (!step.recovery_pending && step.retry_timer.is_none())
        }
        _ => false,
    }
}

fn followup<'a>(state: &'a ExecutionState, step: &StepState) -> Option<&'a Followup> {
    state.node(&step.step_id)?.followup.as_ref()
}

/// When the step's next reminder is due, if one is pending. Nothing is due
/// while the run is paused or the step isn't open.
pub(crate) fn reminder_due(
    state: &ExecutionState,
    step: &StepState,
) -> Result<Option<i64>, EngineError> {
    if state.status != ExecutionStatus::Running || !is_open(step) {
        return Ok(None);
    }
    let (Some(f), Some(opened)) = (followup(state, step), step.opened_at_ms) else {
        return Ok(None);
    };
    let Some(after) = f.remind_after else {
        return Ok(None);
    };
    let first = crate::deadline::add(opened, after.millis)?;
    if step.reminders_sent == 0 {
        return Ok(Some(first));
    }
    let Some(every) = f.remind_every else {
        return Ok(None);
    };
    let later = every
        .millis
        .checked_mul(u64::from(step.reminders_sent))
        .ok_or(EngineError::new("deadline.overflow"))?;
    Ok(Some(crate::deadline::add(first, later)?))
}

/// When the step escalates, if it hasn't yet.
pub(crate) fn escalation_due(
    state: &ExecutionState,
    step: &StepState,
) -> Result<Option<i64>, EngineError> {
    if state.status != ExecutionStatus::Running
        || !is_open(step)
        || step.followup_escalation.is_some()
    {
        return Ok(None);
    }
    let (Some(f), Some(opened)) = (followup(state, step), step.opened_at_ms) else {
        return Ok(None);
    };
    let Some(after) = f.escalate_after else {
        return Ok(None);
    };
    Ok(Some(crate::deadline::add(opened, after.millis)?))
}

pub(crate) fn escalation_request(
    state: &ExecutionState,
    step: &StepState,
    due_at_ms: i64,
) -> Result<EscalationRequest, EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let targets = runbook_spec::resolve_followup_escalation(state.compiled(), node)
        .map_err(|_| EngineError::new("step.no_escalation_target"))?;
    Ok(EscalationRequest {
        base_sequence: state.sequence,
        step_run_id: step.id.clone().ok_or(EngineError::new("step.not_found"))?,
        deadline_at_ms: due_at_ms,
        targets,
    })
}

/// Record the reminder due at `due_at_ms`; refuses one that isn't due.
pub(crate) fn remind(
    state: &mut ExecutionState,
    step_run_id: &StepRunId,
    due_at_ms: i64,
    at_ms: i64,
) -> Result<usize, EngineError> {
    let index = state.step_index(step_run_id)?;
    if reminder_due(state, &state.steps[index])? != Some(due_at_ms) || at_ms < due_at_ms {
        return Err(EngineError::new("step.invalid_reminder"));
    }
    // Reminders that fell due together (after a pause, say) send once: the
    // count jumps to every reminder due by now, so the next is in the future.
    let mut sent = state.steps[index]
        .reminders_sent
        .checked_add(1)
        .ok_or(EngineError::new("step.reminder_overflow"))?;
    if let Some(every) = followup(state, &state.steps[index]).and_then(|f| f.remind_every) {
        let overdue = u64::try_from(at_ms - due_at_ms).unwrap_or(0) / every.millis.max(1);
        sent = sent.saturating_add(u32::try_from(overdue).unwrap_or(u32::MAX));
    }
    state.steps[index].reminders_sent = sent;
    Ok(index)
}

/// Record a step's follow-up escalation; refuses one that isn't due or
/// doesn't match its declaration.
pub(crate) fn escalate(
    state: &mut ExecutionState,
    snapshot: &EscalationSnapshot,
    at_ms: i64,
) -> Result<usize, EngineError> {
    let index = state.step_index(&snapshot.request.step_run_id)?;
    let step = &state.steps[index];
    let due = escalation_due(state, step)?;
    if due != Some(snapshot.request.deadline_at_ms)
        || at_ms < snapshot.request.deadline_at_ms
        || snapshot.request != escalation_request(state, step, snapshot.request.deadline_at_ms)?
        || snapshot.directory_revision == 0
        || snapshot
            .recipients
            .iter()
            .any(|r| !snapshot.request.targets.contains(r))
    {
        return Err(EngineError::new("step.invalid_escalation"));
    }
    state.steps[index].followup_escalation = Some(snapshot.clone());
    Ok(index)
}

/// After every event: a step that has just become open (or opened for a new
/// attempt) records when, and starts its reminders and escalation afresh.
/// Steps are matched by their run IDs, so a reordered step list can't reset
/// an open step.
pub(crate) fn note_open(previous: &ExecutionState, next: &mut ExecutionState, at_ms: i64) {
    let was_open: BTreeMap<&StepRunId, (bool, u32)> = previous
        .steps
        .iter()
        .filter_map(|s| s.id.as_ref().map(|id| (id, (is_open(s), s.attempt))))
        .collect();
    for step in &mut next.steps {
        if !is_open(step) {
            continue;
        }
        let continues = step
            .id
            .as_ref()
            .and_then(|id| was_open.get(id))
            .is_some_and(|&(open, attempt)| open && attempt == step.attempt);
        if !continues || step.opened_at_ms.is_none() {
            step.opened_at_ms = Some(at_ms);
            step.reminders_sent = 0;
            step.followup_escalation = None;
        }
    }
}

/// Who a reminder is for: the step's current recipients (approvers, a
/// strict or handed-off assignee, the person who claimed it, its declared
/// assignee, its escalation), or the runbook's owners when there are none.
pub(crate) fn reminder_recipients(state: &ExecutionState, step: &StepState) -> Vec<RecipientRef> {
    let mut out: Vec<RecipientRef> = Vec::new();
    let mut add = |r: &RecipientRef| {
        if !out.contains(r) {
            out.push(r.clone());
        }
    };
    if let Some(approval) = &step.approval {
        approval
            .requested
            .iter()
            .flat_map(|r| r.recipients.iter())
            .for_each(&mut add);
        approval
            .escalation
            .iter()
            .flat_map(|e| e.recipients.iter())
            .for_each(&mut add);
    }
    if let Some(resolved) = &step.strict_assignee {
        resolved.eligible_recipients.iter().for_each(&mut add);
    }
    if let Some(handoff) = &step.handoff {
        handoff.eligible_recipients.iter().for_each(&mut add);
    }
    if step.status == StepStatus::Claimed
        && let Some(ActorRef::Human { user_id }) = &step.actor
    {
        add(&RecipientRef::User(user_id.clone()));
    }
    if let Some(NodeKind::Task(task)) = state.node(&step.step_id).map(|n| &n.kind)
        && let Some(Assignee::Recipient(recipient)) = &task.assignee
    {
        resolve(state, std::slice::from_ref(recipient))
            .iter()
            .for_each(&mut add);
    }
    if let Some(escalation) = &step.followup_escalation {
        escalation.recipients.iter().for_each(&mut add);
    }
    if out.is_empty() {
        out = resolve(state, &state.compiled().ir().metadata.owners);
    }
    out
}

/// The users and teams named, as the compiled runbook resolved them.
fn resolve(state: &ExecutionState, recipients: &[Recipient]) -> Vec<RecipientRef> {
    let compiled = state.compiled();
    recipients
        .iter()
        .filter_map(|recipient| match recipient {
            Recipient::User(name) => compiled.users().get(name).cloned().map(RecipientRef::User),
            Recipient::Team(name) => compiled
                .teams()
                .get(name)
                .map(|id| RecipientRef::Team(id.to_string())),
        })
        .collect()
}
