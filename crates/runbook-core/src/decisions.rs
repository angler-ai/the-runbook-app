//! Approval decisions (B6a): human votes authorized against the directory
//! snapshot read inside the session (D28), agent decisions with agent
//! provenance only (D26).
use crate::{
    ClaimAction, ClaimWrite,
    actor::{ActorError, ActorPrincipal, ActorWork},
    blob::BlobStore,
    identity::vote_eligibility,
    session::SessionStore,
    start::StartStore,
};
use runbook_model::{
    ActorKind, ActorRef, ApprovalAgentNote, ClaimId, ClaimRef, DecisionLeaseView, DecisionRecord,
    DecisionRequest, RecipientRef, RunnerAbstainRequest, RunnerDecideRequest, RunnerId, StepRunId,
};

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
    let step = &work.runtime.state.steps[work.step_index(step_run_id)?];
    if runbook_engine::independent_review_unknown_service(&work.runtime.state, step)
        && work
            .runtime
            .directory
            .project_role(&work.runtime.scope.project_id, user)
            == Some(runbook_model::ProjectRole::Admin)
    {
        return Err(ActorError::rejected("approval.unknown_service_contributor"));
    }
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
pub async fn agent_decide<S: SessionStore + StartStore + BlobStore>(
    store: &S,
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
    if crate::calls::caller_reaches_destructive(store, &work.runtime.state).await? {
        return Err(ActorError::rejected("approval.invalid_agent_policy"));
    }
    let response = apply_agent(
        &mut work,
        principal,
        step_run_id,
        request.option.clone(),
        request.comment.clone(),
        vec![],
        session_id,
    )?;
    work.settle()?;
    match work
        .commit_replay(vec![], replay, &operation, &response)
        .await?
    {
        crate::CommitResult::Committed(_) => Ok(response),
        crate::CommitResult::Conflict => Err(ActorError::rejected("approval.conflict")),
    }
}

fn apply_agent<S: SessionStore>(
    work: &mut ActorWork<S>,
    principal: &ActorPrincipal,
    step_run_id: &StepRunId,
    option: String,
    comment: Option<String>,
    evidence: Vec<String>,
    session_id: &str,
) -> Result<DecisionRecord, ActorError> {
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
        audit_only_comment: true,
        at_ms: work.at_ms(),
        base_sequence: work.runtime.state.sequence,
        step_run_id: step_run_id.clone(),
        option,
        comment,
        evidence,
        principal: principal_id.clone(),
        agent_name: name.clone(),
        model: model.clone(),
        session_id: session_id.to_owned(),
    };
    for event in runbook_engine::apply_agent_decision(&work.runtime.state, &fact)? {
        work.append(event, principal.actor.clone())?;
    }
    Ok(crate::execution_document::agent_record(&fact))
}

pub(crate) async fn claim_decision<S: SessionStore>(
    mut work: ActorWork<S>,
    runner: &RunnerId,
    entry: &runbook_model::AgentDirectoryEntry,
    model: &str,
    step_run_id: &StepRunId,
    claim_id: ClaimId,
    replay: Option<&crate::actor::Replay>,
) -> Result<ClaimRef, ActorError> {
    let principal = crate::grants::runner_key(runner);
    let owner = ActorPrincipal {
        actor: ActorRef::Agent {
            principal_id: principal.clone(),
            session_id: claim_id.to_string(),
            name: entry.name.clone(),
            model: Some(model.to_owned()),
            service_attribution: None,
        },
        credential_ref: principal.clone(),
        principal,
    };
    let operation = format!("claim_step:{step_run_id}");
    if let Some(saved) = work.replayed(&owner, replay, &operation).await? {
        return Ok(saved);
    }
    if !work.runtime.state.pause_reasons.is_empty() {
        return Err(ActorError::rejected("execution.paused"));
    }
    let previous = work.snapshot.claims.get(step_run_id);
    if previous.is_some_and(|claim| !claim.released) {
        return Err(ActorError::rejected("step.not_claimable"));
    }
    let expected = previous.map_or(0, |claim| claim.generation);
    let generation = expected
        .checked_add(1)
        .and_then(std::num::NonZeroU64::new)
        .ok_or(ActorError::rejected("claim.generation_exhausted"))?;
    work.append(
        runbook_engine::Event::DecisionLeaseClaimed {
            step_run_id: step_run_id.clone(),
            lease: DecisionLeaseView {
                lease_id: claim_id.clone(),
                agent_name: entry.name.clone(),
            },
            agent_version_id: entry.version_id.clone(),
        },
        owner.actor.clone(),
    )?;
    work.settle()?;
    let write = ClaimWrite {
        execution_id: work.runtime.state.id.clone(),
        step_run_id: step_run_id.clone(),
        expected_generation: expected,
        claim_id: claim_id.clone(),
        generation: generation.get(),
        action: ClaimAction::Claim,
        owner: owner.actor.clone(),
        session: owner.session(),
        lease_deadline_at_ms: Some(work.at_ms() + crate::claims::AGENT_LEASE_MS),
        released: false,
    };
    let response = ClaimRef {
        id: claim_id,
        generation,
    };
    match work
        .commit_replay(vec![write], replay, &operation, &response)
        .await?
    {
        crate::CommitResult::Committed(_) => Ok(response),
        crate::CommitResult::Conflict => Err(ActorError::rejected("claim.conflict")),
    }
}

pub enum DecisionOutcome {
    Decide(RunnerDecideRequest),
    Abstain(RunnerAbstainRequest),
}

pub async fn finish_decision<S: SessionStore + StartStore + BlobStore>(
    store: &S,
    mut work: ActorWork<S>,
    runner: &RunnerId,
    step_run_id: &StepRunId,
    lease_id: &ClaimId,
    outcome: DecisionOutcome,
    replay: &crate::actor::Replay,
) -> Result<serde_json::Value, ActorError> {
    let operation = match &outcome {
        DecisionOutcome::Decide(_) => format!("runner_decide:{lease_id}"),
        DecisionOutcome::Abstain(_) => format!("runner_abstain:{lease_id}"),
    };
    let key = crate::grants::runner_key(runner);
    let claim = work
        .snapshot
        .claims
        .get(step_run_id)
        .filter(|c| c.claim_id == *lease_id && c.session.principal == key)
        .ok_or(ActorError::rejected("claim.not_found"))?;
    let principal = ActorPrincipal {
        principal: key,
        actor: claim.owner.clone(),
        credential_ref: claim.session.credential_ref.clone(),
    };
    if let Some(saved) = work.replayed(&principal, Some(replay), &operation).await? {
        return Ok(saved);
    }
    let state = &work.runtime.state;
    let step = &state.steps[work.step_index(step_run_id)?];
    if step.approval.is_none() {
        return Err(ActorError::rejected("step.not_approval"));
    }
    if state
        .pause_reasons
        .contains(&runbook_engine::PauseReason::Manual)
    {
        return Err(ActorError::rejected("execution.paused"));
    }
    // Check closure before the lease fence, so a losing decision gets approval.not_open.
    runbook_engine::admit_decision(state, step, work.at_ms())?;
    if claim.released
        || claim.lease_deadline_at_ms.is_none_or(|t| t <= work.at_ms())
        || step
            .approval
            .as_ref()
            .and_then(|a| a.decision_lease.as_ref())
            .is_none_or(|l| l.lease_id != *lease_id)
    {
        return Err(ActorError::rejected("claim.stale_generation"));
    }
    let ActorRef::Agent {
        model,
        name,
        principal_id,
        ..
    } = &principal.actor
    else {
        return Err(ActorError::rejected("approval.agent_required"));
    };
    let response = match outcome {
        DecisionOutcome::Decide(r) => {
            if crate::calls::caller_reaches_destructive(store, &work.runtime.state).await? {
                return Err(ActorError::rejected("approval.invalid_agent_policy"));
            }
            if r.comment.trim().is_empty()
                || r.comment.len() > 4096
                || r.evidence.is_empty()
                || r.evidence.len() > 64
                || !r.evidence.iter().all(|item| evidence_ref(item))
            {
                return Err(ActorError::rejected("approval.invalid_evidence"));
            }
            let comment = redacted_line(r.comment);
            serde_json::to_value(apply_agent(
                &mut work,
                &principal,
                step_run_id,
                r.option,
                Some(comment),
                r.evidence,
                lease_id.as_str(),
            )?)
            .map_err(|_| ActorError::rejected("request.invalid"))?
        }
        DecisionOutcome::Abstain(r) => {
            if r.reason.trim().is_empty() || r.reason.len() > 4096 {
                return Err(ActorError::rejected("request.invalid"));
            }
            let note = ApprovalAgentNote {
                lease_id: lease_id.clone(),
                principal: principal_id.clone(),
                agent_name: name.clone(),
                model: model.clone(),
                reason: redacted_line(r.reason),
                at_ms: work.at_ms(),
            };
            work.append(
                runbook_engine::Event::ApprovalAgentAbstained {
                    step_run_id: step_run_id.clone(),
                    note: note.clone(),
                },
                principal.actor.clone(),
            )?;
            serde_json::to_value(note).map_err(|_| ActorError::rejected("request.invalid"))?
        }
    };
    work.settle()?;
    match work
        .commit_replay(vec![], Some(replay), &operation, &response)
        .await?
    {
        crate::CommitResult::Committed(_) => Ok(response),
        crate::CommitResult::Conflict => Err(ActorError::rejected("approval.conflict")),
    }
}

/// Reconcile a settled ledger total against the lease that opened the
/// decision, even after the person won or a replacement runner took over.
pub async fn record_decision_cost<S: SessionStore>(
    mut work: ActorWork<S>,
    step_run_id: &StepRunId,
    lease_id: &ClaimId,
    reported: &runbook_model::StepCost,
) -> Result<(), ActorError> {
    use runbook_engine::Event;
    if decision_cost_recorded(&work.snapshot.events, step_run_id, lease_id) {
        return Ok(());
    }
    let actor = work
        .snapshot
        .events
        .iter()
        .find_map(|envelope| match &envelope.event {
            Event::DecisionLeaseClaimed {
                step_run_id: step,
                lease,
                ..
            } if step == step_run_id && lease.lease_id == *lease_id => Some(envelope.actor.clone()),
            _ => None,
        })
        .ok_or(ActorError::rejected("claim.not_found"))?;
    let cost = runbook_engine::RecordedCost::from_cost(reported)
        .filter(|_| reported.valid())
        .ok_or(ActorError::rejected("request.invalid"))?;
    work.append(
        Event::StepCostReported {
            step_run_id: step_run_id.clone(),
            cost,
            claim_id: None,
        },
        actor,
    )?;
    match work.commit(vec![]).await? {
        crate::CommitResult::Committed(_) => Ok(()),
        crate::CommitResult::Conflict => Err(ActorError::rejected("approval.conflict")),
    }
}

fn decision_cost_recorded(
    events: &[runbook_engine::EventEnvelope],
    step_run_id: &StepRunId,
    lease_id: &ClaimId,
) -> bool {
    events.iter().any(|envelope| {
        matches!(&envelope.event, runbook_engine::Event::StepCostReported { step_run_id: step, .. } if step == step_run_id)
            && matches!(&envelope.actor, ActorRef::Agent { principal_id, session_id, .. }
                if principal_id.starts_with("runner:") && session_id == lease_id.as_str())
    })
}

fn evidence_ref(item: &str) -> bool {
    item.strip_prefix("[call ")
        .and_then(|item| item.strip_suffix(']'))
        .is_some_and(|number| {
            number
                .parse::<u32>()
                .is_ok_and(|n| n > 0 && n.to_string() == number)
        })
}

fn redacted_line(text: String) -> String {
    let mut out = crate::redact::redact_lines(vec![text])
        .pop()
        .unwrap_or_default();
    let mut end = out.len().min(4096);
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    out
}

/// The rendered question, options and model constraints shared by ready and claimed reads.
pub fn decision_input(
    state: &runbook_engine::ExecutionState,
    step: &runbook_engine::StepState,
    approval: &runbook_spec::ir::Approval,
) -> runbook_model::ReadyDecision {
    let limits = approval.agent_limits.as_ref();
    runbook_model::ReadyDecision {
        prompt: runbook_engine::approval_prompt(state, step).unwrap_or_default(),
        options: approval
            .options
            .iter()
            .map(|option| runbook_model::ApprovalOption {
                id: option.id.clone(),
                title: option.title.clone(),
                auto: option.auto,
            })
            .collect(),
        model_profile: limits.and_then(|l| l.model.clone()),
        max_tokens: limits.and_then(|l| l.max_tokens),
    }
}
