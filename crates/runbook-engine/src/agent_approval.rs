//! Trusted runtime agent identity is persisted separately from human signatures.
use crate::{EngineError, Event, ExecutionState};
use runbook_model::{ActorRef, StepRunId, StepStatus};
use runbook_spec::ir::NodeKind;
use serde::{Deserialize, Serialize};

/// Untagged to retain the established automatic DecisionApplied wire payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AppliedDecision {
    Automatic(crate::AutomaticDecision),
    Agent(AgentDecision),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDecision {
    /// New agent comments stay in the audit record. The default preserves
    /// outputs when replaying events recorded before this field existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub audit_only_comment: bool,
    pub at_ms: i64,
    pub base_sequence: u64,
    pub step_run_id: StepRunId,
    pub option: String,
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    pub principal: String,
    pub agent_name: String,
    pub model: Option<String>,
    pub session_id: String,
}
impl AgentDecision {
    pub fn actor(&self) -> ActorRef {
        ActorRef::Agent {
            principal_id: self.principal.clone(),
            session_id: self.session_id.clone(),
            name: self.agent_name.clone(),
            model: self.model.clone(),
        }
    }
}
fn check(state: &ExecutionState, fact: &AgentDecision) -> Result<usize, EngineError> {
    crate::authorize_at_deadline(state, fact.at_ms)?;
    if !crate::deadline::open(state) {
        return Err(EngineError::new("execution.not_running"));
    }
    if fact.base_sequence != state.sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    let index = state.step_index(&fact.step_run_id)?;
    let step = &state.steps[index];
    if state.phase == crate::ExecutionPhase::Cleanup
        && !crate::cleanup::is_cleanup_step(state, step)?
    {
        return Err(EngineError::new("execution.cleanup_only"));
    }
    let Some(NodeKind::Approval(approval)) = state.node(&step.step_id).map(|n| &n.kind) else {
        return Err(EngineError::new("step.not_approval"));
    };
    if !approval.allow_agents || approval.min != 1 {
        return Err(EngineError::new("approval.invalid_agent_policy"));
    }
    let record = step
        .approval
        .as_ref()
        .ok_or(EngineError::invariant("approval state is absent"))?;
    if record.automatic_pending {
        return Err(EngineError::new("approval.automatic_pending"));
    }
    if !matches!(step.status, StepStatus::Waiting | StepStatus::Expired)
        || record.decision.is_some()
        || record.agent_decision.is_some()
        || record.automatic_decision.is_some()
        || crate::approval::decision(state, step)?.is_some()
    {
        return Err(EngineError::new("approval.not_open"));
    }
    if fact.principal.trim().is_empty()
        || fact.agent_name.trim().is_empty()
        || fact.session_id.trim().is_empty()
        || fact.model.as_ref().is_some_and(|m| m.trim().is_empty())
    {
        return Err(EngineError::new("approval.invalid_agent_provenance"));
    }
    if !approval.options.iter().any(|o| o.id == fact.option) {
        return Err(EngineError::new("approval.invalid_option"));
    }
    Ok(index)
}
/// Identity and authorization of the agent credential are supplied by trusted core.
/// Agents decide min=1 gates directly and never add a human quorum signature.
pub fn apply_agent_decision(
    state: &ExecutionState,
    fact: &AgentDecision,
) -> Result<Vec<Event>, EngineError> {
    let index = check(state, fact)?;
    if state.steps[index]
        .approval
        .as_ref()
        .and_then(|record| record.requested.as_ref())
        .and_then(|requested| requested.request.agent_ready_at_ms)
        .is_some_and(|ready_at| fact.at_ms < ready_at)
    {
        return Err(EngineError::new("approval.agent_not_ready"));
    }
    // Policy code is consulted when the decision is made, never on replay (F5).
    if !runbook_spec::validate_policy(state.compiled()).is_empty() {
        return Err(EngineError::new("approval.invalid_agent_policy"));
    }
    crate::approval::admit(state, &state.steps[index], fact.at_ms)?;
    Ok(vec![Event::DecisionApplied {
        decision: AppliedDecision::Agent(fact.clone()),
    }])
}
pub(crate) fn apply(
    state: &mut ExecutionState,
    fact: &AgentDecision,
    at_ms: i64,
    actor: &ActorRef,
) -> Result<usize, EngineError> {
    if actor != &fact.actor() {
        return Err(EngineError::new("approval.actor_mismatch"));
    }
    if at_ms != fact.at_ms {
        return Err(EngineError::new("approval.time_mismatch"));
    }
    let index = check(state, fact)?;
    let step = &mut state.steps[index];
    step.approval
        .as_mut()
        .ok_or(EngineError::invariant("approval state disappeared"))?
        .agent_decision = Some(fact.clone());
    step.status = StepStatus::Succeeded;
    step.actor = Some(fact.actor());
    step.finished_at_ms = Some(at_ms);
    step.outputs = [
        ("decision".into(), serde_json::json!(fact.option)),
        (
            "comment".into(),
            serde_json::json!(if fact.audit_only_comment {
                ""
            } else {
                fact.comment.as_deref().unwrap_or("")
            }),
        ),
        ("decided_by".into(), serde_json::json!(fact.principal)),
    ]
    .into();
    state
        .pause_reasons
        .remove(&crate::PauseReason::ApprovalExpired(
            fact.step_run_id.clone(),
        ));
    crate::deadline::sync_pause(state);
    Ok(index)
}
