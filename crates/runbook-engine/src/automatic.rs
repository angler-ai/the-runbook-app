//! Automatic policy produces system provenance, never a human signature.
use crate::{EngineError, Event, ExecutionState};
use runbook_model::{ActorRef, StepRunId, StepStatus};
use runbook_spec::ir::{ApprovalTimeout, NodeKind, Recipient};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeoutPolicy {
    pub step_run_id: StepRunId,
    pub deadline_at_ms: i64,
    pub at_ms: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAuthor {
    System,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionPolicy {
    OnTimeout,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomaticDecision {
    pub base_sequence: u64,
    pub timeout: TimeoutPolicy,
    pub option: String,
    pub by: DecisionAuthor,
    pub policy: DecisionPolicy,
}
pub(crate) fn validate(
    state: &ExecutionState,
    policy: &TimeoutPolicy,
) -> Result<(usize, String), EngineError> {
    if !crate::deadline::open(state) {
        return Err(EngineError::new("execution.not_running"));
    }
    crate::authorize_at_deadline(state, policy.deadline_at_ms)?;
    let index = state.step_index(&policy.step_run_id)?;
    let step = &state.steps[index];
    if step.deadline_at_ms != Some(policy.deadline_at_ms) || policy.at_ms < policy.deadline_at_ms {
        return Err(EngineError::new("approval.automatic_not_due"));
    }
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::invariant("approval declaration is absent"))?;
    let NodeKind::Approval(approval) = &node.kind else {
        return Err(EngineError::new("step.not_approval"));
    };
    let ApprovalTimeout::Decide(option) = &approval.on_timeout else {
        return Err(EngineError::new("approval.not_automatic"));
    };
    let record = step
        .approval
        .as_ref()
        .ok_or(EngineError::invariant("approval state is absent"))?;
    if record.decision.is_some()
        || record.automatic_decision.is_some()
        || !matches!(step.status, StepStatus::Waiting | StepStatus::Expired)
    {
        return Err(EngineError::new("approval.not_open"));
    }
    if step.status == StepStatus::Expired && !record.automatic_pending {
        return Err(EngineError::new("approval.not_automatic"));
    }
    Ok((index, option.clone()))
}
/// Commit this ordered batch atomically. The second event has no client-supplied
/// actor identity, and is fenced to the first event's resulting sequence.
pub fn apply_automatic_decision(
    state: &ExecutionState,
    policy: &TimeoutPolicy,
) -> Result<Vec<Event>, EngineError> {
    let (index, option) = validate(state, policy)?;
    let expire = state.steps[index].status == StepStatus::Waiting;
    // Compile is also used for editable drafts. Check the sealed publication
    // policy when the decision is made, so a draft cannot bypass auto-option or
    // destructive-path restrictions. The reducer never re-runs this check:
    // replay must not depend on today's policy code (F5). A refused decision
    // is withheld; the gate expires undecided and `on_expired` applies.
    if !runbook_spec::validate_policy(state.compiled()).is_empty() {
        if !expire {
            return Err(EngineError::new("approval.invalid_automatic_policy"));
        }
        return Ok(vec![Event::AutomaticDecisionWithheld {
            step_run_id: policy.step_run_id.clone(),
            deadline_at_ms: policy.deadline_at_ms,
            reason: "approval.invalid_automatic_policy".into(),
        }]);
    }
    let base_sequence = state
        .sequence
        .checked_add(u64::from(expire))
        .ok_or(EngineError::invariant("event sequence overflow"))?;
    let mut events = vec![];
    if expire {
        events.push(Event::ApprovalExpired {
            step_run_id: policy.step_run_id.clone(),
            deadline_at_ms: policy.deadline_at_ms,
        });
    }
    events.push(Event::DecisionApplied {
        decision: crate::AppliedDecision::Automatic(AutomaticDecision {
            base_sequence,
            timeout: policy.clone(),
            option,
            by: DecisionAuthor::System,
            policy: DecisionPolicy::OnTimeout,
        }),
    });
    Ok(events)
}
pub(crate) fn expire(
    state: &mut ExecutionState,
    policy: &TimeoutPolicy,
) -> Result<usize, EngineError> {
    let (index, _) = validate(state, policy)?;
    let step = &mut state.steps[index];
    if step.status != StepStatus::Waiting {
        return Err(EngineError::new("approval.not_expiring"));
    }
    step.status = StepStatus::Expired;
    let record = step
        .approval
        .as_mut()
        .ok_or(EngineError::invariant("approval state disappeared"))?;
    record.expired = true;
    record.automatic_pending = true;
    Ok(index)
}
pub(crate) fn apply(
    state: &mut ExecutionState,
    fact: &AutomaticDecision,
    at_ms: i64,
) -> Result<usize, EngineError> {
    if fact.timeout.at_ms != at_ms {
        return Err(EngineError::new("approval.time_mismatch"));
    }
    let (index, option) = validate(state, &fact.timeout)?;
    if fact.base_sequence != state.sequence
        || fact.option != option
        || !state.steps[index]
            .approval
            .as_ref()
            .is_some_and(|a| a.automatic_pending)
    {
        return Err(EngineError::new("approval.invalid_automatic_decision"));
    }
    let recipients = approvers(state, index)?;
    let step = &mut state.steps[index];
    let record = step
        .approval
        .as_mut()
        .ok_or(EngineError::invariant("approval state disappeared"))?;
    record.automatic_pending = false;
    record.automatic_decision = Some(fact.clone());
    record.notices.push(crate::ApprovalNotice {
        step_run_id: fact.timeout.step_run_id.clone(),
        kind: crate::ApprovalNoticeKind::Expired,
        recipients,
        directory_revision: None,
    });
    step.status = StepStatus::Succeeded;
    step.actor = Some(ActorRef::System);
    step.finished_at_ms = Some(at_ms);
    step.outputs = [
        ("decision".into(), serde_json::json!(option)),
        ("comment".into(), serde_json::json!("")),
        ("decided_by".into(), serde_json::json!("system")),
    ]
    .into();
    Ok(index)
}
/// The gate's pinned approvers, named by the expiry notice.
fn approvers(
    state: &ExecutionState,
    index: usize,
) -> Result<Vec<runbook_model::RecipientRef>, EngineError> {
    let node = state
        .node(&state.steps[index].step_id)
        .ok_or(EngineError::invariant("approval declaration is absent"))?;
    let NodeKind::Approval(approval) = &node.kind else {
        return Err(EngineError::new("step.not_approval"));
    };
    approval
        .approvers
        .iter()
        .map(|recipient| {
            match recipient {
                Recipient::User(name) => state
                    .compiled()
                    .users()
                    .get(name)
                    .cloned()
                    .map(runbook_model::RecipientRef::User),
                Recipient::Team(name) => state
                    .compiled()
                    .teams()
                    .get(name)
                    .map(|id| runbook_model::RecipientRef::Team(id.to_string())),
            }
            .ok_or(EngineError::invariant("approval recipient pin is absent"))
        })
        .collect()
}
/// Reduce a withheld automatic decision: the gate expires undecided, like an
/// escalated gate that nobody decided, and `on_expired` applies. Whether the
/// decision should have been withheld was settled when it was recorded.
pub(crate) fn withhold(
    state: &mut ExecutionState,
    policy: &TimeoutPolicy,
    reason: &str,
) -> Result<usize, EngineError> {
    if reason.is_empty() || reason.len() > 128 {
        return Err(EngineError::new("approval.invalid_withheld_reason"));
    }
    let (index, _) = validate(state, policy)?;
    if state.steps[index].status != StepStatus::Waiting {
        return Err(EngineError::new("approval.not_expiring"));
    }
    let recipients = approvers(state, index)?;
    let NodeKind::Approval(approval) = &state
        .node(&state.steps[index].step_id)
        .ok_or(EngineError::invariant("approval declaration is absent"))?
        .kind
    else {
        return Err(EngineError::new("step.not_approval"));
    };
    let pause = approval.on_expired == runbook_spec::ir::ExpiredPolicy::Pause;
    let step = &mut state.steps[index];
    step.status = StepStatus::Expired;
    let record = step
        .approval
        .as_mut()
        .ok_or(EngineError::invariant("approval state disappeared"))?;
    record.expired = true;
    record.automatic_withheld = Some(reason.to_owned());
    record.notices.push(crate::ApprovalNotice {
        step_run_id: policy.step_run_id.clone(),
        kind: crate::ApprovalNoticeKind::Expired,
        recipients,
        directory_revision: None,
    });
    if pause {
        state
            .pause_reasons
            .insert(crate::PauseReason::ApprovalExpired(
                policy.step_run_id.clone(),
            ));
        crate::deadline::sync_pause(state);
    }
    Ok(index)
}
pub(crate) fn pending(
    state: &ExecutionState,
    at_ms: i64,
) -> Result<Option<crate::Command>, EngineError> {
    for step in &state.steps {
        if step.approval.as_ref().is_some_and(|a| a.automatic_pending) {
            return Ok(Some(crate::Command::ApplyAutomaticDecision {
                policy: TimeoutPolicy {
                    step_run_id: step
                        .id
                        .clone()
                        .ok_or(EngineError::invariant("approval has no run ID"))?,
                    deadline_at_ms: step
                        .deadline_at_ms
                        .ok_or(EngineError::invariant("automatic approval has no deadline"))?,
                    at_ms,
                },
            }));
        }
    }
    Ok(None)
}
