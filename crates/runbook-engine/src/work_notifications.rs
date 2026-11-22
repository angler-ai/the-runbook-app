//! Notification inputs are persisted facts; delivery never runs in this engine.
use crate::{Command, EngineError, ExecutionState, StepState};
use runbook_model::{ActorKind, ReadyWork, RecipientRef, StepRunId, StepStatus};
use runbook_spec::ir::{NodeKind, Recipient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkReason {
    #[default]
    Initial,
    ApprovalDecided,
    Timer,
    Signal,
    AutoDecision,
    #[serde(rename = "lease_expired")]
    LeaseExpiry,
    Cleanup,
}
impl WorkReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::ApprovalDecided => "approval_decided",
            Self::Timer => "timer",
            Self::Signal => "signal",
            Self::AutoDecision => "auto_decision",
            Self::LeaseExpiry => "lease_expired",
            Self::Cleanup => "cleanup",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub base_sequence: u64,
    pub step_run_id: StepRunId,
    pub targets: Vec<RecipientRef>,
    /// When the runner may first seek a decision lease.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_ready_at_ms: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequestSnapshot {
    pub request: ApprovalRequest,
    /// Trusted runtime resolves directory membership; this never authorizes voting.
    pub recipients: Vec<RecipientRef>,
    pub directory_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentAvailability {
    pub base_sequence: u64,
    pub reason: WorkReason,
    pub ready_steps: Vec<ReadyWork>,
    #[serde(default, with = "crate::notification::optional_stored_value")]
    pub params: Option<Value>,
    pub reads: Vec<runbook_expr::ReadRef>,
}
fn approval_request(
    state: &ExecutionState,
    step: &StepState,
) -> Result<ApprovalRequest, EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::invariant("approval node absent"))?;
    let NodeKind::Approval(gate) = &node.kind else {
        return Err(EngineError::new("step.not_approval"));
    };
    let targets = gate
        .approvers
        .iter()
        .map(|recipient| {
            match recipient {
                Recipient::User(name) => state
                    .compiled()
                    .users()
                    .get(name)
                    .cloned()
                    .map(RecipientRef::User),
                Recipient::Team(name) => state
                    .compiled()
                    .teams()
                    .get(name)
                    .map(|id| RecipientRef::Team(id.to_string())),
            }
            .ok_or(EngineError::invariant("approval recipient pin absent"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ApprovalRequest {
        base_sequence: state.sequence,
        step_run_id: step
            .id
            .clone()
            .ok_or(EngineError::invariant("approval run ID absent"))?,
        targets,
        agent_ready_at_ms: gate
            .agent_target
            .as_ref()
            .map(|_| {
                crate::deadline::add(
                    step.opened_at_ms.unwrap_or(state.at_ms),
                    gate.agent_after.map_or(0, |after| after.millis),
                )
            })
            .transpose()?,
    })
}
pub(crate) fn pending_approval(state: &ExecutionState) -> Result<Option<Command>, EngineError> {
    if !crate::deadline::open(state) {
        return Ok(None);
    }
    for step in &state.steps {
        if step.status == StepStatus::Waiting
            && step
                .approval
                .as_ref()
                .is_some_and(|a| a.requested.is_none())
        {
            return Ok(Some(Command::ResolveApprovalRequest {
                request: approval_request(state, step)?,
            }));
        }
    }
    Ok(None)
}
pub(crate) fn record_request(
    state: &mut ExecutionState,
    snapshot: &ApprovalRequestSnapshot,
) -> Result<(), EngineError> {
    if !crate::deadline::open(state) {
        return Err(EngineError::new("execution.not_running"));
    }
    let index = state.step_index(&snapshot.request.step_run_id)?;
    let step = &state.steps[index];
    if step.status != StepStatus::Waiting
        || step.approval.as_ref().is_none_or(|a| a.requested.is_some())
        || approval_request(state, step)? != snapshot.request
        || snapshot.directory_revision == 0
    {
        return Err(EngineError::new("notification.invalid_request"));
    }
    let has_team = snapshot
        .request
        .targets
        .iter()
        .any(|r| matches!(r, RecipientRef::Team(_)));
    if snapshot.recipients.len() > 10_000
        || snapshot
            .recipients
            .iter()
            .map(|r| match r {
                RecipientRef::User(id) => (0, id.as_str()),
                RecipientRef::Team(id) => (1, id.as_str()),
            })
            .collect::<BTreeSet<_>>()
            .len()
            != snapshot.recipients.len()
        || snapshot.recipients.iter().any(|r| {
            !snapshot.request.targets.contains(r)
                && !(has_team && matches!(r, RecipientRef::User(_)))
        })
    {
        return Err(EngineError::new("notification.invalid_recipients"));
    }
    state.steps[index]
        .approval
        .as_mut()
        .ok_or(EngineError::invariant("approval state absent"))?
        .requested = Some(snapshot.clone());
    Ok(())
}
fn pending_steps(state: &ExecutionState) -> Result<Vec<&StepState>, EngineError> {
    if state.status != runbook_model::ExecutionStatus::Running {
        return Ok(vec![]);
    }
    let frontier = crate::tree::frontier(state)?;
    let mut steps: Vec<_> = frontier.into_iter().filter(|s| s.status == StepStatus::Ready && !s.human_continuation
        && s.agent_notified_attempt != Some(s.attempt)
        // LANG7: a strict `actor: any` task is its assignee's (a person's).
        && matches!(state.node(&s.step_id).map(|n| &n.kind),Some(NodeKind::Task(t)) if crate::assignment::agents_may_claim(t)))
        .collect();
    // One readiness fact per notice bounds title/provenance copying and keeps
    // independently caused work (ordinary versus cleanup) distinguishable.
    steps.truncate(1);
    Ok(steps)
}
fn reason(state: &ExecutionState, steps: &[&StepState]) -> WorkReason {
    if state.phase == crate::ExecutionPhase::Cleanup
        || steps.iter().any(|s| {
            state
                .node(&s.step_id)
                .is_some_and(|n| n.source_path.contains("/finally/"))
        })
    {
        WorkReason::Cleanup
    } else {
        state.work_reason
    }
}
pub(crate) fn pending_agent(state: &ExecutionState) -> Result<Option<Command>, EngineError> {
    let steps = pending_steps(state)?;
    if steps.is_empty() {
        return Ok(None);
    }
    let mut reads = vec![];
    let mut ready_steps = vec![];
    for step in &steps {
        let node = state
            .node(&step.step_id)
            .ok_or(EngineError::invariant("ready node absent"))?;
        // The shared step-title rendering: a failing template falls back to
        // the raw title instead of blocking the notice.
        let title = crate::presentation::rendered_title(state, step, &mut reads)
            .unwrap_or_else(|| node.id.clone());
        let (agent_id, group_id) = match &node.kind {
            NodeKind::Task(task) => match &task.agent_target {
                Some(runbook_spec::ir::AgentTarget::Runner {
                    agent_id, group_id, ..
                }) => (Some(agent_id.clone()), Some(group_id.clone())),
                _ => (None, None),
            },
            _ => (None, None),
        };
        ready_steps.push(ReadyWork {
            step_run_id: step
                .id
                .clone()
                .ok_or(EngineError::invariant("ready task ID absent"))?,
            path: step.step_id.clone(),
            title,
            actor_kind: ActorKind::Agent,
            agent_id,
            group_id,
        });
    }
    Ok(Some(Command::NotifyAgentWork {
        work: AgentAvailability {
            base_sequence: state.sequence,
            reason: reason(state, &steps),
            ready_steps,
            params: state
                .compiled()
                .ir()
                .agent_params
                .clone()
                .map(Value::Object),
            reads,
        },
    }))
}
pub(crate) fn record_agent(
    state: &mut ExecutionState,
    work: &AgentAvailability,
) -> Result<(), EngineError> {
    let expected = pending_steps(state)?;
    if expected.is_empty()
        || work.base_sequence != state.sequence
        || work.reason != reason(state, &expected)
        || work.ready_steps.len() != expected.len()
        || work.params.as_ref().and_then(Value::as_object)
            != state.compiled().ir().agent_params.as_ref()
        || (work.params.is_some() && work.params.as_ref().is_none_or(|v| !v.is_object()))
        || work.ready_steps.iter().zip(&expected).any(|(view, step)| {
            step.id.as_ref() != Some(&view.step_run_id)
                || view.path != step.step_id
                || view.actor_kind != ActorKind::Agent
                || view.title.len() > 65_536
        })
        || work.reads.len() > 4096
    {
        return Err(EngineError::new("notification.invalid_agent_work"));
    }
    let ids = work
        .ready_steps
        .iter()
        .map(|s| s.step_run_id.clone())
        .collect::<Vec<_>>();
    for id in ids {
        let index = state.step_index(&id)?;
        state.steps[index].agent_notified_attempt = Some(state.steps[index].attempt);
    }
    Ok(())
}
