//! Runtime directory resolution is a supplied fact, not an engine-side lookup.
use crate::{Command, EngineError, ExecutionState, StepState};
use runbook_model::{RecipientRef, ReportStatus, StepRunId, StepStatus};
use runbook_spec::ir::{Assignee, NodeKind, Recipient};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffRequest {
    pub step_run_id: StepRunId,
    pub base_sequence: u64,
    pub attempt: u32,
    pub reason: ReportStatus,
    pub assignee: Option<Recipient>,
    pub owners: Vec<Recipient>,
    pub reads: Vec<runbook_expr::ReadRef>,
    /// LANG7: a strict task hands off to its assignee only, never to the
    /// owners. Absent (false) on every request recorded before LANG7.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffSource {
    Assignee,
    Owners,
    Unresolved,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedHandoff {
    pub request: HandoffRequest,
    pub source: HandoffSource,
    pub eligible_recipients: Vec<RecipientRef>,
    pub notification_recipients: Vec<RecipientRef>,
    pub directory_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedNotification {
    pub step_run_id: StepRunId,
    pub attempt: u32,
    pub reason: ReportStatus,
    pub recipients: Vec<RecipientRef>,
    pub routing_error: Option<String>,
    pub directory_revision: u64,
}
impl BlockedNotification {
    pub fn event_type(&self) -> &'static str {
        "step.blocked"
    }
}
pub(crate) fn request(
    state: &ExecutionState,
    step: &StepState,
) -> Result<HandoffRequest, EngineError> {
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
        Some(Assignee::Expression(_)) => {
            let value = crate::data::evaluate_scoped_path(
                state,
                &step.scope,
                &format!("{}/assignee", node.source_path),
                &mut reads,
            )?;
            Some(parse_recipient(
                value
                    .as_str()
                    .ok_or(EngineError::new("handoff.invalid_assignee"))?,
            )?)
        }
    };
    Ok(HandoffRequest {
        step_run_id: step.id.clone().ok_or(EngineError::new("step.not_found"))?,
        base_sequence: state.sequence,
        attempt: step.attempt,
        reason: step
            .attempt_history
            .last()
            .ok_or(EngineError::new("step.no_attempt"))?
            .report
            .status,
        assignee,
        owners: state.compiled().ir().metadata.owners.clone(),
        reads,
        strict: task.assignment.is_strict(),
    })
}
pub(crate) fn parse_recipient(text: &str) -> Result<Recipient, EngineError> {
    let (kind, name) = text
        .split_once(':')
        .ok_or(EngineError::new("handoff.invalid_assignee"))?;
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(EngineError::new("handoff.invalid_assignee"));
    }
    match kind {
        "user" => Ok(Recipient::User(name.into())),
        "team" => Ok(Recipient::Team(name.into())),
        _ => Err(EngineError::new("handoff.invalid_assignee")),
    }
}
pub(crate) fn recipients_valid(recipients: &[RecipientRef]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    recipients.iter().all(|r| {
        let key = match r {
            RecipientRef::User(id) => (0, id.as_str()),
            RecipientRef::Team(name) => (1, name.as_str()),
        };
        !key.1.is_empty()
            && key.1.len() <= 128
            && !key.1.chars().any(char::is_control)
            && seen.insert(key)
    })
}
pub(crate) fn apply(
    state: &mut ExecutionState,
    handoff: &ResolvedHandoff,
    at_ms: i64,
) -> Result<usize, EngineError> {
    let request = &handoff.request;
    if request.base_sequence != state.sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    let index = state.step_index(&request.step_run_id)?;
    let step = &state.steps[index];
    if step.status != StepStatus::Waiting
        || !step.recovery_pending
        || step.attempt != request.attempt
        || crate::retry::failure_action_for(state, step)? != crate::FailureAction::Handoff
    {
        return Err(EngineError::new("handoff.not_pending"));
    }
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::Task(task) = &node.kind else {
        return Err(EngineError::new("step.not_task"));
    };
    if request.owners != state.compiled().ir().metadata.owners
        || request.strict != task.assignment.is_strict()
        || (request.strict && handoff.source == HandoffSource::Owners)
        || request.reason
            != step
                .attempt_history
                .last()
                .ok_or(EngineError::new("step.no_attempt"))?
                .report
                .status
    {
        return Err(EngineError::new("handoff.invalid_resolution"));
    }
    match &task.assignee {
        None if request.assignee.is_some() => {
            return Err(EngineError::new("handoff.invalid_resolution"));
        }
        Some(Assignee::Recipient(recipient)) if request.assignee.as_ref() != Some(recipient) => {
            return Err(EngineError::new("handoff.invalid_resolution"));
        }
        Some(Assignee::Expression(_)) if request.assignee.is_none() => {
            return Err(EngineError::new("handoff.invalid_resolution"));
        }
        _ => (),
    }
    let unresolved = handoff.source == HandoffSource::Unresolved;
    if handoff.directory_revision == 0
        || (!unresolved && handoff.notification_recipients.is_empty())
        || !recipients_valid(&handoff.notification_recipients)
        || !recipients_valid(&handoff.eligible_recipients)
        || unresolved != handoff.eligible_recipients.is_empty()
        || (!unresolved && handoff.notification_recipients != handoff.eligible_recipients)
        || (handoff.source == HandoffSource::Assignee && request.assignee.is_none())
        || (handoff.source == HandoffSource::Owners && request.owners.is_empty())
    {
        return Err(EngineError::new("handoff.invalid_resolution"));
    }
    let step = &mut state.steps[index];
    step.recovery_pending = false;
    if !unresolved {
        crate::retry::ready_next(step)?;
        step.human_continuation = true;
    }
    step.routing_error = unresolved.then(|| "handoff.no_recipient".into());
    step.handoff = Some(handoff.clone());
    step.blocked_notification_queued = false;
    crate::data::append_reads(&mut step.reads, request.reads.iter().cloned());
    crate::retry::rearm(state, index, at_ms)?;
    Ok(index)
}
pub(crate) fn notification(step: &StepState) -> Option<BlockedNotification> {
    let Some(handoff) = step.handoff.as_ref() else {
        return crate::assignment::notification(step);
    };
    Some(BlockedNotification {
        step_run_id: handoff.request.step_run_id.clone(),
        attempt: handoff.request.attempt,
        reason: handoff.request.reason,
        recipients: handoff.notification_recipients.clone(),
        routing_error: step.routing_error.clone(),
        directory_revision: handoff.directory_revision,
    })
}
pub(crate) fn pending_notifications(state: &ExecutionState) -> Vec<Command> {
    state
        .steps
        .iter()
        .filter(|s| !s.blocked_notification_queued)
        .filter_map(notification)
        .map(|notification| Command::NotifyStepBlocked { notification })
        .collect()
}
