//! R0-4: runner claims and release grants (runner design §6.3, §7.5).
//!
//! A runner-targeted step is claimed only with the claim token of a runner
//! in its agent's group; the claim re-checks the agent's current ceiling
//! (refused with `claim.outside_ceiling` and a recorded waiting reason
//! when the step no longer fits) and pins the agent version. A grant is a
//! JWT bound to the runner's live claim: its `exp` is the lease deadline,
//! and it is refused once the claim moved on (`grant.claim_moved`), a
//! person took the step over, or the lease lapsed (`grant.lease_expired`).
//! A credential grant must be within the step's `connections` narrowing
//! and the agent's published ceiling, and records `CredentialReleased`
//! with its delivery mode. No secret value ever passes through here.
use crate::{
    actor::{ActorError, ActorPrincipal, ActorWork, Replay},
    runners::RunnerPrincipal,
    session::SessionStore,
    signing::{GRANT_TYP, GrantClaims, JwtSigner, KeyPurpose, KeyRing},
};
use runbook_engine::{AgentTargetRef, Event};
use runbook_model::{
    ActorRef, AgentDirectoryEntry, AgentStatus, ClaimId, ClaimRef, ClaimRequest, Delivery,
    GrantKind, GrantRequest, StepRunId, StepStatus,
};
use runbook_spec::ir::{NodeKind, Task};

/// A signed grant and its expiry (the lease deadline, epoch ms).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedGrant {
    pub grant: String,
    pub expires_at_ms: i64,
}

struct WorkPolicy<'a> {
    target: Option<&'a runbook_spec::ir::AgentTarget>,
    limits: Option<&'a runbook_spec::ir::AgentLimits>,
    connections: Option<&'a Vec<String>>,
    timeout_ms: Option<u64>,
    destructive: bool,
    decision: bool,
}

fn policy_of<'a, S: SessionStore>(
    work: &'a ActorWork<S>,
    step_run_id: &StepRunId,
) -> Result<WorkPolicy<'a>, ActorError> {
    let step = &work.runtime.state.steps[work.step_index(step_run_id)?];
    let node = work
        .runtime
        .state
        .node(&step.step_id)
        .ok_or(ActorError::rejected("step.not_found"))?;
    match &node.kind {
        NodeKind::Task(task) => Ok(WorkPolicy {
            target: task.agent_target.as_ref(),
            limits: task.agent_limits.as_ref(),
            connections: task.connections.as_ref(),
            timeout_ms: node.timeout.map(|duration| duration.millis),
            destructive: task.destructive,
            decision: false,
        }),
        NodeKind::Approval(approval) => Ok(WorkPolicy {
            target: approval.agent_target.as_ref(),
            limits: approval.agent_limits.as_ref(),
            connections: None,
            timeout_ms: None,
            destructive: false,
            decision: true,
        }),
        _ => Err(ActorError::rejected("step.not_task")),
    }
}

/// The runner agent resolved for a task or approval.
pub fn target_agent<S: SessionStore>(
    work: &ActorWork<S>,
    step_run_id: &StepRunId,
) -> Result<Option<String>, ActorError> {
    let index = work.step_index(step_run_id)?;
    Ok(match &work.runtime.state.steps[index].runner.target {
        Some(AgentTargetRef::Runner { agent_id }) => Some(agent_id.clone()),
        Some(AgentTargetRef::External(_)) => None,
        None => match policy_of(work, step_run_id)?.target {
            Some(runbook_spec::ir::AgentTarget::Runner { agent_id, .. }) => Some(agent_id.clone()),
            _ => None,
        },
    })
}

/// Whether the step still fits the agent's current ceiling (§6.3: limits,
/// model profile, hosts, credentials and destructive reach).
pub fn fits_ceiling(task: &Task, entry: &AgentDirectoryEntry) -> bool {
    fits_policy(
        &WorkPolicy {
            target: task.agent_target.as_ref(),
            limits: task.agent_limits.as_ref(),
            connections: task.connections.as_ref(),
            timeout_ms: None,
            destructive: task.destructive,
            decision: false,
        },
        entry,
    )
}

fn fits_policy(policy: &WorkPolicy<'_>, entry: &AgentDirectoryEntry) -> bool {
    let ceiling = &entry.ceiling;
    let empty = runbook_spec::ir::AgentLimits::default();
    let limits = policy.limits.unwrap_or(&empty);
    let model = limits
        .model
        .as_ref()
        .is_none_or(|m| ceiling.profiles.contains(m));
    let cost = limits
        .max_cost_usd
        .is_none_or(|c| c <= ceiling.max_cost_usd);
    let tokens = limits.max_tokens.is_none_or(|t| t <= ceiling.max_tokens);
    let hosts = limits
        .network
        .iter()
        .flatten()
        .all(|h| ceiling.hosts.iter().any(|c| &c.host == h));
    let credentials = policy
        .connections
        .into_iter()
        .flatten()
        .all(|id| ceiling.credentials.iter().any(|c| &c.id == id));
    let reach = runbook_model::RunnerLabel::Destructive
        != ceiling
            .tools
            .iter()
            .map(|t| t.label)
            .chain(
                ceiling
                    .credentials
                    .iter()
                    .filter(|c| {
                        c.delivery.contains(&Delivery::Model)
                            || policy
                                .connections
                                .as_ref()
                                .is_none_or(|ids| ids.contains(&c.id))
                    })
                    .map(|c| c.label),
            )
            .chain(
                ceiling
                    .hosts
                    .iter()
                    .filter(|h| {
                        limits
                            .network
                            .as_ref()
                            .is_none_or(|hosts| hosts.contains(&h.host))
                    })
                    .map(|h| h.label),
            )
            .max()
            .unwrap_or(runbook_model::RunnerLabel::Read)
        || policy.destructive
        || policy.decision;
    model && cost && tokens && hosts && credentials && reach
}

/// Record why a ready step waits (once per reason) and commit.
async fn record_waiting<S: SessionStore>(
    mut work: ActorWork<S>,
    step_run_id: &StepRunId,
    reason: &str,
    detail: Option<&str>,
) -> Result<(), ActorError> {
    let index = work.step_index(step_run_id)?;
    let step = &work.runtime.state.steps[index];
    if (step.status != StepStatus::Ready
        && !runbook_engine::decision_open(&work.runtime.state, step))
        || (step.runner.waiting_reason.as_deref() == Some(reason)
            && step.runner.waiting_detail.as_deref() == detail)
    {
        return Ok(());
    }
    work.append(
        Event::StepWaiting {
            step_run_id: step_run_id.clone(),
            reason: reason.to_owned(),
            detail: detail.map(str::to_owned),
        },
        ActorRef::System,
    )?;
    work.settle()?;
    work.commit(vec![]).await?;
    Ok(())
}

fn decision_profile<'a>(
    policy: &WorkPolicy<'_>,
    entry: &'a AgentDirectoryEntry,
) -> Option<(&'a String, &'a String)> {
    let profile = match policy.limits.and_then(|limits| limits.model.as_ref()) {
        Some(name) => entry.ceiling.profile_models.get_key_value(name)?,
        None => entry
            .ceiling
            .profile_models
            .get_key_value(entry.ceiling.profiles.first()?)?,
    };
    Some(profile)
}

/// A runner's claim of a step: the step must target an agent of the
/// runner's group and the run's project, active and still within its
/// current ceiling. The claim
/// pins the agent's current version.
#[allow(clippy::too_many_arguments)]
pub async fn runner_claim<S: SessionStore>(
    mut work: ActorWork<S>,
    runner: &RunnerPrincipal,
    decision_supported: bool,
    entry: Option<&AgentDirectoryEntry>,
    principal: &ActorPrincipal,
    step_run_id: &StepRunId,
    request: &ClaimRequest,
    claim_id: ClaimId,
    replay: Option<&Replay>,
) -> Result<ClaimRef, ActorError> {
    let agent = target_agent(&work, step_run_id)?;
    let policy = policy_of(&work, step_run_id)?;
    let entry = match (agent, entry) {
        (Some(agent), Some(entry))
            if entry.agent_id == agent
                && Some(&entry.group_id) == runner.group_id.as_ref()
                && entry.project_id == work.runtime.scope.project_id =>
        {
            entry
        }
        _ => return Err(ActorError::rejected("claim.wrong_runner")),
    };
    if entry.status != AgentStatus::Active {
        record_waiting(work, step_run_id, "agent_paused", None).await?;
        return Err(ActorError::rejected("claim.agent_paused"));
    }
    if !fits_policy(&policy, entry) {
        record_waiting(work, step_run_id, "agent_outside_ceiling", None).await?;
        return Err(ActorError::rejected("claim.outside_ceiling"));
    }
    // §6.5 rule 2: publish proved an approval precedes a destructive step
    // on every path; the claim checks a person actually decided it in
    // this run, with an option permitted by the approval's `approves` list when declared.
    let index = work.step_index(step_run_id)?;
    if policy.destructive
        && !runbook_engine::person_approved(&work.runtime.state, &work.runtime.state.steps[index])
    {
        return Err(ActorError::rejected("claim.approval_missing"));
    }
    if policy.decision {
        if !decision_supported {
            return Err(ActorError::rejected("runner.decision_unsupported"));
        }
        let runner_id = runner
            .runner_id
            .as_ref()
            .ok_or(ActorError::rejected("claim.wrong_runner"))?;
        if request.take_over {
            return Err(ActorError::rejected("claim.wrong_runner"));
        }
        let Some((_, model)) = decision_profile(&policy, entry) else {
            record_waiting(
                work,
                step_run_id,
                "agent_outside_ceiling",
                Some("Decision model unavailable"),
            )
            .await?;
            return Err(ActorError::rejected("claim.outside_ceiling"));
        };
        return crate::decisions::claim_decision(
            work,
            runner_id,
            entry,
            model,
            step_run_id,
            claim_id,
            replay,
        )
        .await;
    }
    work.runner_pin = Some(entry.version_id.clone());
    crate::claims::claim_step(work, principal, step_run_id, request, claim_id, replay).await
}

/// The principal key a runner's claim is held under.
pub fn runner_key(runner_id: &runbook_model::RunnerId) -> String {
    format!("runner:{runner_id}")
}

/// What a grant needs beside the record.
pub struct GrantContext<'a> {
    /// The install's release token holder.
    pub runner: &'a RunnerPrincipal,
    /// The group of the runner the request names (`None`: not one of the
    /// install's runners).
    pub runner_group: Option<runbook_model::RunnerGroupId>,
    pub decision_supported: bool,
    pub entry: Option<&'a AgentDirectoryEntry>,
    pub keys: &'a KeyRing,
    pub issuer: &'a str,
    pub jti: String,
}

/// Issue (or renew) a grant for the runner's live claim of a step.
pub async fn issue<S: SessionStore>(
    mut work: ActorWork<S>,
    context: GrantContext<'_>,
    step_run_id: &StepRunId,
    request: &GrantRequest,
) -> Result<SignedGrant, ActorError> {
    let not_allowed = || ActorError::rejected("grant.not_allowed");
    let moved = || ActorError::rejected("grant.claim_moved");
    let runner_group = context.runner_group.clone().ok_or_else(not_allowed)?;
    let claim = work
        .snapshot
        .claims
        .get(step_run_id)
        .cloned()
        .ok_or_else(moved)?;
    let index = work.step_index(step_run_id)?;
    let policy = policy_of(&work, step_run_id)?;
    let step = &work.runtime.state.steps[index];
    if policy.decision && !context.decision_supported {
        return Err(not_allowed());
    }
    if claim.released
        || claim.claim_id != request.claim_id
        || claim.generation != request.generation
        || claim.session.principal != runner_key(&request.runner_id)
        || if policy.decision {
            runbook_engine::admit_decision(&work.runtime.state, step, work.at_ms()).is_err()
                || step
                    .approval
                    .as_ref()
                    .and_then(|a| a.decision_lease.as_ref())
                    .is_none_or(|lease| lease.lease_id != claim.claim_id)
        } else {
            step.status != StepStatus::Claimed
        }
    {
        return Err(moved());
    }
    let deadline = claim.lease_deadline_at_ms.ok_or_else(moved)?;
    if deadline <= work.at_ms() {
        return Err(ActorError::rejected("grant.lease_expired"));
    }
    // The agent as it is now: still the run project's, active, and the
    // step still within its current ceiling (first grant and renewal).
    let agent = target_agent(&work, step_run_id)?.ok_or_else(not_allowed)?;
    let entry = context
        .entry
        .filter(|e| {
            e.agent_id == agent
                && e.group_id == runner_group
                && e.project_id == work.runtime.scope.project_id
                && e.status == AgentStatus::Active
                && fits_policy(&policy, e)
        })
        .ok_or_else(not_allowed)?;
    let agent_version = step
        .runner
        .agent_version_id
        .clone()
        .unwrap_or_else(|| entry.version_id.clone());
    let decision_model = if policy.decision {
        let (profile, model) = decision_profile(&policy, entry).ok_or_else(not_allowed)?;
        if !matches!(&claim.owner, ActorRef::Agent { model: Some(pinned), .. } if pinned == model) {
            return Err(not_allowed());
        }
        Some((profile.clone(), model.clone()))
    } else {
        None
    };
    let mut claims = GrantClaims {
        iss: context.issuer.to_owned(),
        aud: format!("runner:{}", request.runner_id),
        sub: step_run_id.to_string(),
        kind: request.kind,
        claim_id: claim.claim_id.to_string(),
        generation: claim.generation,
        runner_id: request.runner_id.to_string(),
        group_id: runner_group.to_string(),
        agent_id: entry.agent_id.clone(),
        agent_version,
        project_id: work.runtime.scope.project_id.to_string(),
        destructive: policy.destructive,
        reach: policy.decision.then_some(runbook_model::RunnerLabel::Read),
        network: if policy.decision {
            Some(
                entry
                    .ceiling
                    .hosts
                    .iter()
                    .filter(|h| h.label == runbook_model::RunnerLabel::Read)
                    .map(|h| h.host.clone())
                    .collect(),
            )
        } else {
            policy.limits.and_then(|limits| limits.network.clone())
        },
        connections: if policy.decision {
            Some(
                entry
                    .ceiling
                    .credentials
                    .iter()
                    .filter(|c| c.label == runbook_model::RunnerLabel::Read)
                    .map(|c| c.id.clone())
                    .collect(),
            )
        } else {
            policy.connections.cloned()
        },
        model_profile: decision_model
            .as_ref()
            .map(|(profile, _)| profile.clone())
            .or_else(|| policy.limits.and_then(|limits| limits.model.clone())),
        model_id: decision_model.map(|(_, model)| model),
        max_tokens: policy.limits.and_then(|limits| limits.max_tokens),
        timeout_ms: policy.timeout_ms,
        step_deadline_ms: step.deadline_at_ms,
        credential_id: None,
        credential_version: None,
        delivery: None,
        iat: work.at_ms() / 1000,
        exp: deadline / 1000,
        jti: context.jti,
    };
    match request.kind {
        GrantKind::Capability => {
            if request.credential_id.is_some()
                || request.credential_version.is_some()
                || request.delivery.is_some()
            {
                return Err(ActorError::rejected("request.invalid"));
            }
        }
        GrantKind::Credential => {
            let (Some(credential), Some(version), Some(delivery)) = (
                request.credential_id.as_ref(),
                request.credential_version,
                request.delivery,
            ) else {
                return Err(ActorError::rejected("request.invalid"));
            };
            let in_ceiling = entry.ceiling.credentials.iter().any(|c| {
                &c.id == credential
                    && c.delivery.contains(&delivery)
                    && (!policy.decision || c.label == runbook_model::RunnerLabel::Read)
            });
            let in_step = delivery == Delivery::Model
                || claims
                    .connections
                    .as_ref()
                    .is_none_or(|ids| ids.contains(credential));
            if !in_ceiling || !in_step {
                return Err(not_allowed());
            }
            claims.credential_id = Some(credential.clone());
            claims.credential_version = Some(version);
            claims.delivery = Some(delivery);
            // One release per claim, credential, version and delivery: a
            // renewal of a live release records nothing new.
            let renewal = work.runtime.state.steps[index]
                .runner
                .releases
                .iter()
                .any(|r| {
                    r.revoked.is_none()
                        && r.claim_id.as_ref() == Some(&claim.claim_id)
                        && &r.credential_id == credential
                        && r.credential_version == version
                        && r.delivery == delivery.as_str()
                });
            if !renewal {
                work.append(
                    Event::CredentialReleased {
                        step_run_id: step_run_id.clone(),
                        credential_id: credential.clone(),
                        credential_version: version,
                        delivery: delivery.as_str().to_owned(),
                        runner_id: request.runner_id.to_string(),
                        claim_id: Some(claim.claim_id.clone()),
                    },
                    ActorRef::System,
                )?;
                work.settle()?;
            }
        }
    }
    let grant = context
        .keys
        .sign(KeyPurpose::Grant, GRANT_TYP, &claims)
        .map_err(|_| ActorError::rejected("signing.unavailable"))?;
    if work.has_pending() {
        match work.commit(vec![]).await? {
            crate::CommitResult::Committed(_) => {}
            crate::CommitResult::Conflict => return Err(moved()),
        }
    }
    Ok(SignedGrant {
        grant,
        expires_at_ms: deadline,
    })
}

/// Record a revocation of a credential released to the step.
pub async fn revoked<S: SessionStore>(
    mut work: ActorWork<S>,
    step_run_id: &StepRunId,
    credential_id: &str,
    reason: &str,
) -> Result<(), ActorError> {
    let index = work.step_index(step_run_id)?;
    let live = work.runtime.state.steps[index]
        .runner
        .releases
        .iter()
        .any(|r| r.credential_id == credential_id && r.revoked.is_none());
    if !live {
        return Ok(());
    }
    work.append(
        Event::CredentialRevoked {
            step_run_id: step_run_id.clone(),
            credential_id: credential_id.to_owned(),
            reason: reason.to_owned(),
        },
        ActorRef::System,
    )?;
    work.settle()?;
    work.commit(vec![]).await?;
    Ok(())
}

/// Record a waiting reason the runner's scheduler reported.
pub async fn waiting<S: SessionStore>(
    work: ActorWork<S>,
    step_run_id: &StepRunId,
    reason: &str,
    detail: Option<&str>,
) -> Result<(), ActorError> {
    record_waiting(work, step_run_id, reason, detail).await
}
