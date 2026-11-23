//! R0-4: runner agent steps in the run's record (runner design §6.3,
//! §7.5). A task's target is resolved at publish; when the step becomes
//! ready the engine records it (`AgentTargetResolved`), so a replay never
//! depends on the directory. The core records the claim's pinned agent
//! version, why a step waits, and credential releases and revocations.
use crate::{Command, EngineError, ExecutionState, StepState};
use runbook_model::StepRunId;
use serde::{Deserialize, Serialize};

/// `"external"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalTarget {
    #[serde(rename = "external")]
    External,
}
/// Who works a step: `{agent_id}` for a runner agent, `"external"` for a
/// step left open to external agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentTargetRef {
    Runner { agent_id: String },
    External(ExternalTarget),
}

/// One credential released to the step's current or an earlier claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialRelease {
    pub credential_id: String,
    pub credential_version: u64,
    pub delivery: String,
    pub runner_id: String,
    /// The claim it was released to (absent on older records).
    pub claim_id: Option<runbook_model::ClaimId>,
    pub at_ms: i64,
    /// Set once revoked, with the reason.
    pub revoked: Option<String>,
}

/// What the record holds about a step's runner work.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunnerStepState {
    pub target: Option<AgentTargetRef>,
    pub trace_id: Option<String>,
    /// The agent version pinned by the current (or last) claim.
    pub agent_version_id: Option<String>,
    /// Why the step waits for its runner (`agent_paused`,
    /// `agent_outside_ceiling`, `runner_offline`, `limit:<level>`),
    /// cleared when it is claimed.
    pub waiting_reason: Option<String>,
    pub waiting_detail: Option<String>,
    pub releases: Vec<CredentialRelease>,
    /// The claim holding releases has reported.
    pub reported: bool,
    /// Credentials were released to a claim that ended without a report.
    pub released_never_reported: bool,
    /// R0-6: what the step's reports said it cost, summed over attempts.
    pub cost: Option<RecordedCost>,
}

/// R0-6: a cost as the record keeps it: dollar amounts in whole
/// micro-dollars (exact sums, and integers survive the stored event's
/// tagged JSON, where floats would not), tokens as counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedCost {
    pub model_micro_usd: u64,
    pub compute_micro_usd: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
}
impl RecordedCost {
    /// `None` for a negative, non-finite or out-of-range amount.
    pub fn from_cost(cost: &runbook_model::StepCost) -> Option<Self> {
        let micros = |usd: f64| {
            (usd.is_finite() && (0.0..=1.0e12).contains(&usd)).then(|| (usd * 1.0e6).round() as u64)
        };
        Some(Self {
            model_micro_usd: micros(cost.model_usd)?,
            compute_micro_usd: micros(cost.compute_usd)?,
            input_tokens: cost.input_tokens,
            output_tokens: cost.output_tokens,
            cached_tokens: cost.cached_tokens,
        })
    }
    pub fn to_cost(self) -> runbook_model::StepCost {
        runbook_model::StepCost {
            model_usd: self.model_micro_usd as f64 / 1.0e6,
            compute_usd: self.compute_micro_usd as f64 / 1.0e6,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cached_tokens: self.cached_tokens,
        }
    }
    pub fn add(&mut self, other: &Self) {
        self.model_micro_usd = self.model_micro_usd.saturating_add(other.model_micro_usd);
        self.compute_micro_usd = self
            .compute_micro_usd
            .saturating_add(other.compute_micro_usd);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(other.cached_tokens);
    }
}

/// The command that records a ready task's target, once.
pub(crate) fn resolve(
    task: &runbook_spec::ir::Task,
    step: &StepState,
) -> Result<Vec<Command>, EngineError> {
    use runbook_spec::ir::AgentTarget;
    if step.runner.target.is_some() {
        return Ok(vec![]);
    }
    let target = match &task.agent_target {
        Some(AgentTarget::Runner { agent_id, .. }) => AgentTargetRef::Runner {
            agent_id: agent_id.clone(),
        },
        Some(AgentTarget::External) => AgentTargetRef::External(ExternalTarget::External),
        Some(AgentTarget::Named { .. }) => {
            return Err(EngineError::invariant("unresolved agent name"));
        }
        None => return Ok(vec![]),
    };
    Ok(vec![Command::ResolveAgentTarget {
        step_id: step.step_id.clone(),
        step_run_id: step
            .id
            .clone()
            .ok_or(EngineError::invariant("ready task ID absent"))?,
        target,
    }])
}

fn index(state: &ExecutionState, step_run_id: &StepRunId) -> Result<usize, EngineError> {
    state.step_index(step_run_id)
}

pub(crate) fn target_resolved(
    next: &mut ExecutionState,
    state: &ExecutionState,
    step_id: &str,
    step_run_id: &StepRunId,
    target: &AgentTargetRef,
) -> Result<(), EngineError> {
    let i = index(state, step_run_id)?;
    if state.steps[i].step_id != step_id || state.steps[i].runner.target.is_some() {
        return Err(EngineError::new("step.target_already_resolved"));
    }
    next.steps[i].runner.target = Some(target.clone());
    Ok(())
}

pub(crate) fn waiting(
    next: &mut ExecutionState,
    state: &ExecutionState,
    step_run_id: &StepRunId,
    reason: &str,
    detail: Option<&str>,
) -> Result<(), EngineError> {
    let i = index(state, step_run_id)?;
    if state.steps[i].status != runbook_model::StepStatus::Ready
        && !decision_open(state, &state.steps[i])
    {
        return Err(EngineError::new("step.not_waiting"));
    }
    next.steps[i].runner.waiting_reason = Some(reason.to_owned());
    next.steps[i].runner.waiting_detail = detail.map(str::to_owned);
    Ok(())
}

pub(crate) fn released(
    next: &mut ExecutionState,
    state: &ExecutionState,
    step_run_id: &StepRunId,
    release: CredentialRelease,
) -> Result<(), EngineError> {
    let i = index(state, step_run_id)?;
    if state.steps[i].status != runbook_model::StepStatus::Claimed
        && !(decision_open(state, &state.steps[i])
            && state.steps[i].approval.as_ref().is_some_and(|a| {
                a.decision_lease
                    .as_ref()
                    .is_some_and(|lease| Some(&lease.lease_id) == release.claim_id.as_ref())
            }))
    {
        return Err(EngineError::new("step.not_claimed"));
    }
    let runner = &mut next.steps[i].runner;
    runner.reported = false;
    runner.releases.push(release);
    Ok(())
}

pub(crate) fn revoked(
    next: &mut ExecutionState,
    state: &ExecutionState,
    step_run_id: &StepRunId,
    credential_id: &str,
    reason: &str,
) -> Result<(), EngineError> {
    let i = index(state, step_run_id)?;
    for release in next.steps[i]
        .runner
        .releases
        .iter_mut()
        .filter(|r| r.credential_id == credential_id && r.revoked.is_none())
    {
        release.revoked = Some(reason.to_owned());
    }
    Ok(())
}

pub(crate) fn cost_reported(
    next: &mut ExecutionState,
    state: &ExecutionState,
    step_run_id: &StepRunId,
    cost: &RecordedCost,
) -> Result<(), EngineError> {
    let i = index(state, step_run_id)?;
    next.steps[i]
        .runner
        .cost
        .get_or_insert_with(Default::default)
        .add(cost);
    Ok(())
}

/// A dominating person-decided gate in this run and iteration must
/// allow the decided option when it declares `approves`.
pub fn person_approved(state: &ExecutionState, step: &StepState) -> bool {
    let Some(approvals) =
        runbook_spec::dominating_approvals(state.compiled().ir()).remove(&step.node_id)
    else {
        return false;
    };
    state.steps.iter().any(|s| {
        approvals.contains(&s.node_id)
            && step.scope.starts_with(&s.scope)
            && s.approval.as_ref().and_then(|a| a.decision.as_ref()).is_some_and(|decision| {
                state.node(&s.step_id).is_some_and(|node| {
                    matches!(&node.kind, runbook_spec::ir::NodeKind::Approval(approval)
                        if approval.approves.as_ref().is_none_or(|options| options.contains(&decision.option)))
                })
            })
    })
}

/// The claim that received releases ended: without a report, flag it.
pub(crate) fn claim_ended(next: &mut ExecutionState, index: usize) {
    let runner = &mut next.steps[index].runner;
    if !runner.releases.is_empty() && !runner.reported {
        runner.released_never_reported = true;
    }
}

/// An approval can keep its decision lease while people are free to vote.
pub fn decision_open(state: &ExecutionState, step: &StepState) -> bool {
    !state.pause_reasons.contains(&crate::PauseReason::Manual)
        && crate::deadline::open(state)
        && step.status == runbook_model::StepStatus::Waiting
        && (state.phase != crate::ExecutionPhase::Cleanup
            || crate::cleanup::is_cleanup_step(state, step).unwrap_or(false))
        && step.approval.as_ref().is_some_and(|approval| {
            !approval.automatic_pending
                && approval.decision.is_none()
                && approval.automatic_decision.is_none()
                && approval.agent_decision.is_none()
                && approval
                    .requested
                    .as_ref()
                    .is_some_and(|r| r.request.agent_ready_at_ms.is_some())
        })
}

pub fn admit_decision(
    state: &ExecutionState,
    step: &StepState,
    at_ms: i64,
) -> Result<(), EngineError> {
    if !decision_open(state, step) {
        return Err(EngineError::new("approval.not_open"));
    }
    if step
        .approval
        .as_ref()
        .and_then(|a| a.requested.as_ref())
        .and_then(|r| r.request.agent_ready_at_ms)
        .is_none_or(|at| at > at_ms)
    {
        return Err(EngineError::new("step.not_claimable"));
    }
    crate::authorize_at_deadline(state, at_ms)?;
    crate::approval::admit(state, step, at_ms)
}
