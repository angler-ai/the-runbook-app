//! Approval decisions (B6a): human votes authorized against the directory
//! snapshot read inside the session (D28), agent decisions with agent
//! provenance only (D26).
use crate::{
    actor::{ActorError, ActorPrincipal, ActorWork},
    identity::vote_eligibility,
    session::SessionStore,
};
use runbook_model::{ActorKind, ActorRef, DecisionRequest, RecipientRef, StepRunId};

fn targets<S: SessionStore>(work: &ActorWork<S>, step_run_id: &StepRunId) -> Vec<RecipientRef> {
    let index = work
        .runtime
        .state
        .steps
        .iter()
        .position(|step| step.id.as_ref() == Some(step_run_id));
    let mut targets = Vec::new();
    if let Some(index) = index
        && let Some(approval) = &work.runtime.state.steps[index].approval
    {
        if let Some(requested) = &approval.requested {
            targets.extend(requested.request.targets.iter().cloned());
        }
        if let Some(escalation) = &approval.escalation {
            targets.extend(escalation.request.targets.iter().cloned());
        }
    }
    targets
}

pub async fn vote<S: SessionStore>(
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    step_run_id: &StepRunId,
    request: &DecisionRequest,
    replay: Option<&crate::actor::Replay>,
) -> Result<runbook_model::DecisionRecord, ActorError> {
    let operation = format!("vote:{}", step_run_id);
    if let Some(saved) = work
        .replayed::<runbook_model::DecisionRecord>(principal, replay, &operation)
        .await?
    {
        return Ok(saved);
    }
    let user = principal
        .user_id()
        .ok_or(ActorError::rejected("approval.not_eligible"))?;
    let eligibility = vote_eligibility(
        &work.runtime.directory,
        &work.runtime.scope.project_id,
        user,
        &targets(&work, step_run_id),
    );
    let resolved = runbook_engine::ResolvedVote {
        at_ms: work.at_ms(),
        base_sequence: work.runtime.state.sequence,
        step_run_id: step_run_id.clone(),
        actor: principal.actor.clone(),
        credential_ref: principal.credential_ref.clone(),
        option: request.option.clone(),
        comment: request.comment.clone(),
        eligibility,
    };
    for event in runbook_engine::apply_vote(&work.runtime.state, &resolved)? {
        work.append(event, principal.actor.clone())?;
    }
    work.settle()?;
    // A duplicate user/option vote is a no-op even with a different request
    // key. Return the immutable signature, never new unrecorded request data.
    let index = work.step_index(step_run_id)?;
    let signed = work.runtime.state.steps[index]
        .approval
        .as_ref()
        .and_then(|a| {
            a.votes
                .iter()
                .find(|s| s.vote.actor == principal.actor && s.vote.option == request.option)
        })
        .ok_or(runbook_engine::EngineError::Invariant {
            context: "accepted vote has no signature",
        })?;
    let response = runbook_model::DecisionRecord::Human {
        user_id: user.clone(),
        membership_revision: signed
            .vote
            .eligibility
            .directory_revision
            .try_into()
            .map_err(|_| runbook_engine::EngineError::Invariant {
                context: "vote signature has no membership revision",
            })?,
        option: signed.vote.option.clone(),
        comment: signed.vote.comment.clone(),
    };
    match work
        .commit_replay(vec![], replay, &operation, &response)
        .await?
    {
        crate::CommitResult::Committed(_) => Ok(response),
        crate::CommitResult::Conflict => Err(ActorError::rejected("approval.conflict")),
    }
}

/// An agent decision on an agent-allowed gate: DecisionApplied with agent
/// provenance, never a human signature or quorum count.
pub async fn agent_decide<S: SessionStore>(
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    step_run_id: &StepRunId,
    request: &DecisionRequest,
    session_id: &str,
    replay: Option<&crate::actor::Replay>,
) -> Result<runbook_model::DecisionRecord, ActorError> {
    let operation = format!("agent_decide:{}", step_run_id);
    if let Some(saved) = work
        .replayed::<runbook_model::DecisionRecord>(principal, replay, &operation)
        .await?
    {
        return Ok(saved);
    }
    let ActorRef::Agent {
        principal_id,
        name,
        model,
        ..
    } = &principal.actor
    else {
        return Err(ActorError::rejected("approval.agent_required"));
    };
    if principal.kind() != Some(ActorKind::Agent) {
        return Err(ActorError::rejected("approval.agent_required"));
    }
    let fact = runbook_engine::AgentDecision {
        at_ms: work.at_ms(),
        base_sequence: work.runtime.state.sequence,
        step_run_id: step_run_id.clone(),
        option: request.option.clone(),
        comment: request.comment.clone(),
        principal: principal_id.clone(),
        agent_name: name.clone(),
        model: model.clone(),
        session_id: session_id.to_owned(),
    };
    for event in runbook_engine::apply_agent_decision(&work.runtime.state, &fact)? {
        work.append(event, principal.actor.clone())?;
    }
    work.settle()?;
    let response = crate::execution_document::agent_record(&fact);
    match work
        .commit_replay(vec![], replay, &operation, &response)
        .await?
    {
        crate::CommitResult::Committed(_) => Ok(response),
        crate::CommitResult::Conflict => Err(ActorError::rejected("approval.conflict")),
    }
}
