//! LANG7: strict assignment and take-over.
//!
//! A task with `assignment: strict` may be claimed by a person only when
//! that person matches its assignee. When such a step becomes ready (each
//! attempt), the engine asks the host to resolve the assignee against the
//! directory, as it does for a handoff, and records the answer. An
//! assignee that resolves to nobody eligible leaves the step with the
//! handoff routing error `handoff.no_recipient` and a blocked notice to
//! the project admins; eligibility is never broadened.
//!
//! Who may claim (LANG7 review round 1): on an `actor: any` or `actor:
//! human` strict task only the assignee, a person, may claim, so agents are
//! neither offered nor allowed it ([`agents_may_claim`]). On an `actor:
//! agent` strict task `actor` still governs agents; the assignee binds only
//! the person taking it over. An assignee that resolved to nobody is
//! resolved again against the live directory when someone claims it; a
//! resolution that names somebody replaces the empty one for that attempt.
//!
//! A person may also take over an `actor: agent` task that is ready (never
//! claimed, or its agent claim expired or was released); the take-over is
//! recorded after the claim so the run log and report show it.
use crate::{Command, EngineError, ExecutionState, StepState};
use runbook_model::{ActorKind, ActorRef, RecipientRef, StepRunId, StepStatus};
use runbook_spec::ir::{Actor, Assignee, NodeKind, Recipient};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssigneeRequest {
    pub step_run_id: StepRunId,
    pub base_sequence: u64,
    pub attempt: u32,
    /// The assignee to resolve; `None` when an expression assignee did not
    /// evaluate to a `user:` or `team:` reference.
    pub assignee: Option<Recipient>,
    pub reads: Vec<runbook_expr::ReadRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAssignee {
    pub request: AssigneeRequest,
    /// The resolved assignee (at most one), when it names at least one
    /// active user with project run access; empty means unresolved.
    pub eligible_recipients: Vec<RecipientRef>,
    /// The project admins told about an unresolved assignee; empty when
    /// resolved (the Inbox routes resolved work).
    pub notification_recipients: Vec<RecipientRef>,
    pub directory_revision: u64,
}
impl ResolvedAssignee {
    pub fn unresolved(&self) -> bool {
        self.eligible_recipients.is_empty()
    }
}

/// LANG7: who took over an agent task, and from what kind of actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakenOver {
    pub by: ActorRef,
    pub at_ms: i64,
    pub previous_actor_kind: ActorKind,
}

/// LANG7: whether an agent may claim (and so is offered) `task` while it
/// is ready and not handed off. A strict assignee is a person, so a strict
/// `actor: any` task is a person's work; on an `actor: agent` task `actor`
/// governs agents and the assignee binds only a person taking it over.
pub fn agents_may_claim(task: &runbook_spec::ir::Task) -> bool {
    match task.actor {
        Actor::Agent => true,
        Actor::Any => !task.assignment.is_strict(),
        Actor::Human => false,
    }
}

fn strict_task(state: &ExecutionState, step: &StepState) -> bool {
    matches!(state.node(&step.step_id).map(|n| &n.kind),
        Some(NodeKind::Task(task)) if task.assignment.is_strict())
}

/// Whether `step` is a strict task whose current attempt still needs its
/// assignee resolved.
fn needs_resolution(state: &ExecutionState, step: &StepState) -> bool {
    step.status == StepStatus::Ready
        && step.id.is_some()
        && !step.human_continuation
        && strict_task(state, step)
        && step
            .strict_assignee
            .as_ref()
            .is_none_or(|r| r.request.attempt != step.attempt)
}

/// Whether `step` is a ready strict task whose current attempt's assignee
/// resolved to nobody: a resolution naming somebody may replace it.
fn reresolvable(state: &ExecutionState, step: &StepState) -> bool {
    step.status == StepStatus::Ready
        && !step.human_continuation
        && strict_task(state, step)
        && step
            .strict_assignee
            .as_ref()
            .is_some_and(|r| r.request.attempt == step.attempt && r.unresolved())
}

pub(crate) fn request(
    state: &ExecutionState,
    step: &StepState,
) -> Result<AssigneeRequest, EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::Task(task) = &node.kind else {
        return Err(EngineError::new("step.not_task"));
    };
    let mut reads = vec![];
    let assignee = match &task.assignee {
        None => None,
        Some(Assignee::Recipient(recipient)) => Some(recipient.clone()),
        // An expression that fails or names no recipient resolves to
        // nobody (a routing error), never to a broader audience.
        Some(Assignee::Expression(_)) => crate::data::evaluate_scoped_path(
            state,
            &step.scope,
            &format!("{}/assignee", node.source_path),
            &mut reads,
        )
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .and_then(|text| crate::handoff::parse_recipient(&text).ok()),
    };
    Ok(AssigneeRequest {
        step_run_id: step.id.clone().ok_or(EngineError::new("step.not_found"))?,
        base_sequence: state.sequence,
        attempt: step.attempt,
        assignee,
        reads,
    })
}

pub(crate) fn pending(state: &ExecutionState) -> Result<Option<Command>, EngineError> {
    for step in &state.steps {
        if needs_resolution(state, step) {
            return Ok(Some(Command::ResolveAssignee {
                request: Box::new(request(state, step)?),
            }));
        }
    }
    Ok(None)
}

pub(crate) fn apply(
    state: &mut ExecutionState,
    resolved: &ResolvedAssignee,
) -> Result<usize, EngineError> {
    let index = state.step_index(&resolved.request.step_run_id)?;
    let step = &state.steps[index];
    let pending =
        needs_resolution(state, step) || (reresolvable(state, step) && !resolved.unresolved());
    if !pending
        || resolved.request.base_sequence != state.sequence
        || request(state, step)? != resolved.request
    {
        return Err(EngineError::new("assignment.not_pending"));
    }
    let unresolved = resolved.unresolved();
    if resolved.directory_revision == 0
        || resolved.eligible_recipients.len() > 1
        || (resolved.request.assignee.is_none() && !unresolved)
        || (!unresolved && !resolved.notification_recipients.is_empty())
        || !crate::handoff::recipients_valid(&resolved.notification_recipients)
        || !crate::handoff::recipients_valid(&resolved.eligible_recipients)
        || resolved.request.reads.len() > 4096
    {
        return Err(EngineError::new("assignment.invalid_resolution"));
    }
    let step = &mut state.steps[index];
    step.strict_assignee = Some(resolved.clone());
    step.routing_error = unresolved.then(|| "handoff.no_recipient".into());
    step.blocked_notification_queued = false;
    crate::data::append_reads(&mut step.reads, resolved.request.reads.iter().cloned());
    Ok(index)
}

/// The blocked notice for a strict step whose assignee resolved to nobody.
pub(crate) fn notification(step: &StepState) -> Option<crate::BlockedNotification> {
    let resolved = step
        .strict_assignee
        .as_ref()
        .filter(|r| r.unresolved() && r.request.attempt == step.attempt)?;
    Some(crate::BlockedNotification {
        step_run_id: resolved.request.step_run_id.clone(),
        attempt: resolved.request.attempt,
        reason: runbook_model::ReportStatus::Blocked,
        recipients: resolved.notification_recipients.clone(),
        routing_error: step.routing_error.clone(),
        directory_revision: resolved.directory_revision,
    })
}

/// LANG7: record that a person took over an agent task they just claimed.
pub(crate) fn take_over(
    state: &mut ExecutionState,
    step_run_id: &StepRunId,
    previous_actor_kind: ActorKind,
    actor: &ActorRef,
    at_ms: i64,
) -> Result<(), EngineError> {
    let index = state.step_index(step_run_id)?;
    let step = &state.steps[index];
    let agent_task = matches!(state.node(&step.step_id).map(|n| &n.kind),
        Some(NodeKind::Task(task)) if task.actor == Actor::Agent);
    if !agent_task
        || step.status != StepStatus::Claimed
        || step.human_continuation
        || step.taken_over.is_some()
        || !matches!(actor, ActorRef::Human { .. })
        || step.actor.as_ref() != Some(actor)
        || previous_actor_kind != ActorKind::Agent
    {
        return Err(EngineError::new("step.not_taken_over"));
    }
    state.steps[index].taken_over = Some(TakenOver {
        by: actor.clone(),
        at_ms,
        previous_actor_kind,
    });
    Ok(())
}
