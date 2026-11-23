//! Human signatures consume authorization facts supplied by the trusted runtime.
use crate::{EngineError, Event, ExecutionState, StepState};
use runbook_model::{ActorRef, RecipientRef, StepRunId, StepStatus, UserId};
use runbook_spec::ir::{Approval, NodeKind, Recipient};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, rc::Rc};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteEligibility {
    /// Runtime checked active user, project role, and current membership together.
    pub eligible: bool,
    pub directory_revision: u64,
    pub matched_recipients: Vec<RecipientRef>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedVote {
    pub at_ms: i64,
    pub base_sequence: u64,
    pub step_run_id: StepRunId,
    pub actor: ActorRef,
    /// Non-secret credential ID for audit; never a token value.
    pub credential_ref: String,
    pub option: String,
    pub comment: Option<String>,
    pub eligibility: VoteEligibility,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalSignature {
    pub vote: ResolvedVote,
    pub at_ms: i64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApprovalState {
    pub requested: Option<crate::ApprovalRequestSnapshot>,
    pub votes: Vec<Rc<ApprovalSignature>>,
    pub distinct_votes: usize,
    pub decision: Option<ApprovalDecision>,
    pub escalation: Option<crate::EscalationSnapshot>,
    pub expired: bool,
    pub automatic_pending: bool,
    pub automatic_decision: Option<crate::AutomaticDecision>,
    /// Why an `on_timeout: decide` gate expired without its automatic decision.
    pub automatic_withheld: Option<String>,
    pub agent_decision: Option<crate::AgentDecision>,
    pub decision_lease: Option<runbook_model::DecisionLeaseView>,
    pub agent_notes: Vec<runbook_model::ApprovalAgentNote>,
    pub notices: Vec<crate::ApprovalNotice>,
    pub queued_notices: BTreeSet<crate::ApprovalNoticeKind>,
    pub escalation_notice_recipients: Option<Vec<RecipientRef>>,
}
pub(crate) fn definition<'a>(
    state: &'a ExecutionState,
    step: &StepState,
) -> Result<&'a Approval, EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::invariant("approval declaration is absent"))?;
    let NodeKind::Approval(approval) = &node.kind else {
        return Err(EngineError::new("step.not_approval"));
    };
    Ok(approval)
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Contributors {
    users: BTreeSet<UserId>,
    agents: BTreeSet<String>,
    managed_agents: BTreeSet<String>,
    unknown_service: bool,
}

fn sponsor(principal: &str) -> Option<UserId> {
    principal.strip_prefix("agent:")?.parse().ok()
}

pub(crate) fn note_contributor(step: &mut StepState, actor: &ActorRef) {
    match actor {
        ActorRef::Human { user_id } => {
            Rc::make_mut(&mut step.contributors)
                .users
                .insert(user_id.clone());
        }
        ActorRef::Agent {
            principal_id,
            service_attribution,
            ..
        } => {
            let contributions = Rc::make_mut(&mut step.contributors);
            if principal_id.starts_with("runner:")
                && let Some(crate::AgentTargetRef::Runner { agent_id }) = &step.runner.target
            {
                contributions.managed_agents.insert(agent_id.clone());
            } else {
                contributions.agents.insert(principal_id.clone());
            }
            contributions.users.extend(sponsor(principal_id));
            if let Some(attribution) = service_attribution {
                contributions.users.extend(attribution.users().cloned());
                contributions.unknown_service |= attribution.creator.is_none();
            } else if principal_id.starts_with("service:") {
                contributions.unknown_service = true;
            }
        }
        ActorRef::System => {}
    }
}

fn reviewed_tasks<'a>(
    state: &'a ExecutionState,
    gate: &'a StepState,
    referenced: &'a [String],
) -> impl Iterator<Item = &'a StepState> {
    state.steps.iter().filter(move |task| {
        referenced.contains(&task.node_id)
            && (state.engine_version < crate::version::SCOPED_INDEPENDENT_APPROVALS
                || !gate.scope.iter().zip(&task.scope).any(|(gate, task)| {
                    gate.parent == task.parent
                        && gate.index != task.index
                        && state
                            .node(&gate.parent)
                            .is_some_and(|parent| matches!(parent.kind, NodeKind::Foreach(_)))
                }))
    })
}

/// People who contributed directly or through agents to the task instances
/// reviewed by this approval.
pub fn excluded_completers(state: &ExecutionState, step: &StepState) -> BTreeSet<UserId> {
    let Ok(approval) = definition(state, step) else {
        return BTreeSet::new();
    };
    if approval.exclude_completers_of.is_empty() {
        return BTreeSet::new();
    }
    reviewed_tasks(state, step, &approval.exclude_completers_of)
        .flat_map(|task| task.contributors.users.iter().cloned())
        .collect()
}

/// A named task that timed out without a report cannot be independently reviewed.
pub fn independent_review_missing_report(state: &ExecutionState, step: &StepState) -> bool {
    let Ok(approval) = definition(state, step) else {
        return false;
    };
    reviewed_tasks(state, step, &approval.exclude_completers_of).any(|task| {
        crate::tree::terminal(task.status)
            && (state.engine_version < crate::version::SCOPED_INDEPENDENT_APPROVALS
                || !task.amended
                || task.outputs.is_empty())
            && task
                .attempt_history
                .last()
                .is_some_and(|attempt| attempt.timed_out)
    })
}

/// Unattributed service work requires an explicitly named non-admin human.
pub fn independent_review_unknown_service(state: &ExecutionState, step: &StepState) -> bool {
    state.engine_version >= crate::version::SCOPED_INDEPENDENT_APPROVALS
        && definition(state, step).is_ok_and(|approval| {
            reviewed_tasks(state, step, &approval.exclude_completers_of)
                .any(|task| task.contributors.unknown_service)
        })
}

fn is_named_approver(state: &ExecutionState, approval: &Approval, user: &UserId) -> bool {
    approval.approvers.iter().any(|recipient| {
        matches!(recipient, Recipient::User(name) if state.compiled().users().get(name) == Some(user))
    })
}

pub(crate) fn check_independent_agent(
    state: &ExecutionState,
    step: &StepState,
    principal: &str,
) -> Result<(), EngineError> {
    let approval = definition(state, step)?;
    if independent_review_missing_report(state, step) {
        return Err(EngineError::new("approval.review_missing_report"));
    }
    if independent_review_unknown_service(state, step) {
        return Err(EngineError::new("approval.unknown_service_contributor"));
    }
    let sponsored_by = sponsor(principal);
    let managed_agent = if principal.starts_with("runner:") {
        match &step.runner.target {
            Some(crate::AgentTargetRef::Runner { agent_id }) => Some(agent_id.as_str()),
            _ => match &approval.agent_target {
                Some(runbook_spec::ir::AgentTarget::Runner { agent_id, .. }) => {
                    Some(agent_id.as_str())
                }
                _ => None,
            },
        }
    } else {
        None
    };
    if !approval.exclude_completers_of.is_empty()
        && reviewed_tasks(state, step, &approval.exclude_completers_of).any(|task| {
            let contributors = &task.contributors;
            contributors.agents.contains(principal)
                || managed_agent.is_some_and(|id| contributors.managed_agents.contains(id))
                || sponsored_by
                    .as_ref()
                    .is_some_and(|user| contributors.users.contains(user))
        })
    {
        return Err(EngineError::new("approval.excluded_completer"));
    }
    Ok(())
}

fn voter(vote: &ResolvedVote) -> Result<&UserId, EngineError> {
    match &vote.actor {
        ActorRef::Human { user_id } => Ok(user_id),
        _ => Err(EngineError::new("approval.not_eligible")),
    }
}
fn check<'a>(
    state: &'a ExecutionState,
    vote: &ResolvedVote,
) -> Result<(usize, &'a ApprovalState), EngineError> {
    crate::authorize_at_deadline(state, vote.at_ms)?;
    if !crate::deadline::open(state) {
        return Err(EngineError::new("execution.not_running"));
    }
    if vote.base_sequence != state.sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    let index = state.step_index(&vote.step_run_id)?;
    let step = &state.steps[index];
    if state.phase == crate::ExecutionPhase::Cleanup
        && !crate::cleanup::is_cleanup_step(state, step)?
    {
        return Err(EngineError::new(
            if state.terminal_reason.as_deref() == Some("timeout") {
                "execution.deadline_exceeded"
            } else {
                "execution.cleanup_only"
            },
        ));
    }
    let approval = definition(state, step)?;
    if !matches!(step.status, StepStatus::Waiting | StepStatus::Expired) {
        return Err(EngineError::new("approval.not_open"));
    }
    let record = step
        .approval
        .as_ref()
        .ok_or(EngineError::invariant("waiting approval has no state"))?;
    if record.automatic_pending {
        return Err(EngineError::new("approval.automatic_pending"));
    }
    let user = voter(vote)?;
    let facts = &vote.eligibility;
    if !facts.eligible || facts.directory_revision == 0 || facts.matched_recipients.is_empty() {
        return Err(EngineError::new("approval.not_eligible"));
    }
    // Directory membership is a supplied fact, but it must match this pinned gate.
    for recipient in &facts.matched_recipients {
        let matches = approval
            .approvers
            .iter()
            .any(|configured| match (configured, recipient) {
                (Recipient::User(name), RecipientRef::User(id)) => {
                    id == user && state.compiled().users().get(name) == Some(id)
                }
                (Recipient::Team(name), RecipientRef::Team(id)) => state
                    .compiled()
                    .teams()
                    .get(name)
                    .is_some_and(|team| team.as_str() == id),
                _ => false,
            });
        let escalated = record
            .escalation
            .as_ref()
            .is_some_and(|snapshot| snapshot.recipients.contains(recipient));
        let valid_user = !matches!(recipient, RecipientRef::User(id) if id != user);
        if !(matches || escalated) || !valid_user {
            return Err(EngineError::new("approval.not_eligible"));
        }
    }
    if !approval.options.iter().any(|o| o.id == vote.option) {
        return Err(EngineError::new("approval.invalid_option"));
    }
    if vote.credential_ref.is_empty() {
        return Err(EngineError::new("approval.missing_credential"));
    }
    Ok((index, record))
}
/// Command-side admission for a new human or agent decision. These rules
/// are checked only when the decision is submitted, never while folding
/// stored events, so logs recorded before them still replay (F95).
///
/// - An expired gate reopens for a late decision only while its expiration
///   pause holds the run (PRD §5). After `on_expired: fail`, the failure has
///   already been routed and the gate stays expired.
/// - On an `on_timeout: decide` gate, timeout handling wins at or after the
///   gate deadline even before its timer is drained, as at the execution
///   deadline (PRD §5): the automatic decision is the first valid one.
pub(crate) fn admit(
    state: &ExecutionState,
    step: &StepState,
    at_ms: i64,
) -> Result<(), EngineError> {
    let id = step
        .id
        .clone()
        .ok_or(EngineError::invariant("approval has no run ID"))?;
    if step.status == StepStatus::Expired
        && !state
            .pause_reasons
            .contains(&crate::PauseReason::ApprovalExpired(id))
    {
        return Err(EngineError::new("approval.not_open"));
    }
    if step.status == StepStatus::Waiting
        && matches!(
            definition(state, step)?.on_timeout,
            runbook_spec::ir::ApprovalTimeout::Decide(_)
        )
        && step
            .deadline_at_ms
            .is_some_and(|deadline| at_ms >= deadline)
    {
        return Err(EngineError::new("approval.deadline_exceeded"));
    }
    Ok(())
}
fn duplicate(record: &ApprovalState, vote: &ResolvedVote) -> bool {
    record
        .votes
        .iter()
        .any(|signed| signed.vote.actor == vote.actor && signed.vote.option == vote.option)
}
/// Core rechecks authorization on every call, including idempotent repeats.
/// One user/option is one immutable signature regardless of credential or team count.
pub fn apply_vote(state: &ExecutionState, vote: &ResolvedVote) -> Result<Vec<Event>, EngineError> {
    let (index, record) = check(state, vote)?;
    let step = &state.steps[index];
    check_independent_vote(state, step, voter(vote)?)?;
    admit(state, step, vote.at_ms)?;
    if duplicate(record, vote) {
        return Ok(vec![]);
    }
    Ok(vec![Event::ApprovalVoted {
        vote: Box::new(vote.clone()),
    }])
}
fn check_independent_vote(
    state: &ExecutionState,
    step: &StepState,
    user: &UserId,
) -> Result<(), EngineError> {
    if independent_review_missing_report(state, step) {
        return Err(EngineError::new("approval.review_missing_report"));
    }
    if independent_review_unknown_service(state, step)
        && !is_named_approver(state, definition(state, step)?, user)
    {
        return Err(EngineError::new("approval.unknown_service_contributor"));
    }
    if excluded_completers(state, step).contains(user) {
        return Err(EngineError::new("approval.excluded_completer"));
    }
    Ok(())
}

pub(crate) fn record_vote(
    state: &mut ExecutionState,
    vote: &ResolvedVote,
    at_ms: i64,
) -> Result<usize, EngineError> {
    if at_ms != vote.at_ms {
        return Err(EngineError::new("approval.time_mismatch"));
    }
    let (index, record) = check(state, vote)?;
    if state.engine_version >= crate::version::INDEPENDENT_APPROVALS {
        check_independent_vote(state, &state.steps[index], voter(vote)?)?;
    }
    if duplicate(record, vote) {
        return Err(EngineError::new("approval.duplicate_vote"));
    }
    let record = state.steps[index]
        .approval
        .as_mut()
        .ok_or(EngineError::invariant("approval state disappeared"))?;
    record.votes.push(Rc::new(ApprovalSignature {
        vote: vote.clone(),
        at_ms,
    }));
    record.distinct_votes = record
        .votes
        .iter()
        .map(|signature| voter(&signature.vote))
        .collect::<Result<BTreeSet<_>, _>>()?
        .len();
    Ok(index)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDecision {
    pub base_sequence: u64,
    pub step_run_id: StepRunId,
    pub option: String,
    pub decided_by: UserId,
    pub comment: Option<String>,
    pub signers: Vec<UserId>,
}
pub(crate) fn decision(
    state: &ExecutionState,
    step: &StepState,
) -> Result<Option<ApprovalDecision>, EngineError> {
    if independent_review_missing_report(state, step) {
        return Ok(None);
    }
    count_decision(state, step, &excluded_completers(state, step))
}

fn count_decision(
    state: &ExecutionState,
    step: &StepState,
    excluded: &BTreeSet<UserId>,
) -> Result<Option<ApprovalDecision>, EngineError> {
    let approval = definition(state, step)?;
    let record = step
        .approval
        .as_ref()
        .ok_or(EngineError::invariant("approval state is absent"))?;
    let unknown_service = independent_review_unknown_service(state, step);
    let mut options: std::collections::BTreeMap<&str, BTreeSet<&UserId>> = Default::default();
    for signature in &record.votes {
        let vote = &signature.vote;
        if record.automatic_pending {
            return Err(EngineError::new("approval.automatic_pending"));
        }
        let user = voter(vote)?;
        if excluded.contains(user) || (unknown_service && !is_named_approver(state, approval, user))
        {
            continue;
        }
        let signers = options.entry(&vote.option).or_default();
        signers.insert(user);
        if signers.len() >= approval.min as usize {
            return Ok(Some(ApprovalDecision {
                base_sequence: state.sequence,
                step_run_id: step
                    .id
                    .clone()
                    .ok_or(EngineError::invariant("approval has no run ID"))?,
                option: vote.option.clone(),
                decided_by: user.clone(),
                comment: vote.comment.clone(),
                signers: signers.iter().map(|id| (*id).clone()).collect(),
            }));
        }
    }
    Ok(None)
}
pub(crate) fn advance(
    state: &ExecutionState,
    step: &StepState,
) -> Result<Vec<crate::Command>, EngineError> {
    Ok(decision(state, step)?
        .map(|decision| crate::Command::DecideApproval { decision })
        .into_iter()
        .collect())
}
pub(crate) fn record_decision(
    state: &mut ExecutionState,
    fact: &ApprovalDecision,
    at_ms: i64,
) -> Result<usize, EngineError> {
    crate::authorize_at_deadline(state, at_ms)?;
    if !crate::deadline::open(state) {
        return Err(EngineError::new("execution.not_running"));
    }
    let index = state.step_index(&fact.step_run_id)?;
    let step = &state.steps[index];
    if !matches!(step.status, StepStatus::Waiting | StepStatus::Expired)
        || (decision(state, step)?.as_ref() != Some(fact)
            && (state.engine_version >= crate::version::INDEPENDENT_APPROVALS
                || count_decision(state, step, &BTreeSet::new())?.as_ref() != Some(fact)))
    {
        return Err(EngineError::new("approval.invalid_decision"));
    }
    let step = &mut state.steps[index];
    step.approval
        .as_mut()
        .ok_or(EngineError::invariant("approval state disappeared"))?
        .decision = Some(fact.clone());
    step.outputs = [
        ("decision".into(), serde_json::json!(fact.option)),
        (
            "comment".into(),
            serde_json::json!(fact.comment.as_deref().unwrap_or("")),
        ),
        (
            "decided_by".into(),
            serde_json::json!(fact.decided_by.as_str()),
        ),
    ]
    .into();
    step.status = StepStatus::Succeeded;
    step.actor = Some(ActorRef::Human {
        user_id: fact.decided_by.clone(),
    });
    step.finished_at_ms = Some(at_ms);
    state
        .pause_reasons
        .remove(&crate::PauseReason::ApprovalExpired(
            fact.step_run_id.clone(),
        ));
    crate::deadline::sync_pause(state);
    Ok(index)
}
