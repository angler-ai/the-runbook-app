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
}
fn definition<'a>(
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
    admit(state, &state.steps[index], vote.at_ms)?;
    if duplicate(record, vote) {
        return Ok(vec![]);
    }
    Ok(vec![Event::ApprovalVoted {
        vote: Box::new(vote.clone()),
    }])
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
    let approval = definition(state, step)?;
    let record = step
        .approval
        .as_ref()
        .ok_or(EngineError::invariant("approval state is absent"))?;
    let mut options: std::collections::BTreeMap<&str, BTreeSet<&UserId>> = Default::default();
    for signature in &record.votes {
        let vote = &signature.vote;
        if record.automatic_pending {
            return Err(EngineError::new("approval.automatic_pending"));
        }
        let user = voter(vote)?;
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
        || decision(state, step)?.as_ref() != Some(fact)
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
