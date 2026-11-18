//! Claim lifecycle (B6a): fenced claim, heartbeat, logs, report, release
//! and lease expiry. Claim generation fences every actor write; an agent
//! lease defaults to ten minutes and a human claim never expires.
mod admin;
use crate::{
    ClaimAction, ClaimWrite, StoreError,
    actor::{ActorError, ActorPrincipal, ActorWork},
    session::SessionStore,
};
pub use admin::admin_release;
use runbook_engine::Event;
use runbook_model::{
    ActorKind, ActorRef, ChecklistRequest, ClaimId, ClaimRef, ClaimRequest, ExecutionId,
    LogsRequest, ReportRequest, StepRunId, StepStatus,
};
use std::future::Future;

pub const AGENT_LEASE_MS: i64 = 10 * 60 * 1000;
/// PRD §17: outputs per step, as encoded JSON bytes.
pub const MAX_STEP_OUTPUT_BYTES: usize = 64 * 1024;
/// PRD §17: log text per step (appended lines plus report logs). Stores
/// fence the running total of appended lines.
pub const MAX_STEP_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// What one log entry counts toward [`MAX_STEP_LOG_BYTES`]: its bytes, and
/// at least one, so blank lines cannot grow a step's log without bound
/// (CLI3 review M6). Every store counts the same way.
pub fn log_entry_bytes(line: &str) -> u64 {
    (line.len() as u64).max(1)
}

/// CF12b review round 1: the most bytes one stored log entry holds (the
/// engine's cap on one string of run data, and a log chunk). A longer line
/// is stored as several entries ([`split_log_lines`]), never refused: an
/// old client sending one long line keeps working, and a redaction that
/// lengthened a line cannot push an entry past it.
pub const MAX_LOG_ENTRY_BYTES: usize = 65_536;

/// CF12b review round 1: the most a report's logs hold as JSON once
/// redacted. The API holds a report's logs to 1 MiB as sent
/// (`field_caps::REPORT_LOG_BYTES`); redaction replaces a short secret
/// with a longer marker, so the event keeps at most this (128 KiB more),
/// and a report past it is refused (`request.value_too_large`, `/logs`).
pub const MAX_REPORT_LOG_JSON_BYTES: usize = 1_048_576 + 131_072;

/// `line` cut into pieces of at most `max` bytes, each ending on a UTF-8
/// character boundary; a line within `max` (an empty one too) is one piece.
pub fn split_log_line(line: &str, max: usize) -> Vec<&str> {
    // A character is at most four bytes, so each piece makes progress.
    let max = max.max(4);
    let mut pieces = Vec::with_capacity(line.len() / max + 1);
    let mut rest = line;
    while rest.len() > max {
        let mut at = max;
        while !rest.is_char_boundary(at) {
            at -= 1;
        }
        let (piece, tail) = rest.split_at(at);
        pieces.push(piece);
        rest = tail;
    }
    pieces.push(rest);
    pieces
}

/// Each line longer than [`MAX_LOG_ENTRY_BYTES`] as several entries, in
/// order (D38: an entry is a whole line, or a piece of one this long).
pub fn split_log_lines(lines: Vec<String>) -> Vec<String> {
    if lines.iter().all(|l| l.len() <= MAX_LOG_ENTRY_BYTES) {
        return lines;
    }
    lines
        .iter()
        .flat_map(|line| split_log_line(line, MAX_LOG_ENTRY_BYTES))
        .map(str::to_owned)
        .collect()
}

/// A report's logs as its event keeps them: redacted as one stream, split
/// into entries, within the step's [`MAX_STEP_LOG_BYTES`] as stored
/// (`step.logs_too_large`; CF12b review round 2: counted after redaction,
/// as the logs route counts), and within [`MAX_REPORT_LOG_JSON_BYTES`] as
/// JSON.
pub fn report_logs(lines: Vec<String>) -> Result<Vec<String>, ActorError> {
    let logs = split_log_lines(crate::redact::redact_lines(lines));
    if logs.iter().map(|line| log_entry_bytes(line)).sum::<u64>() > MAX_STEP_LOG_BYTES {
        return Err(ActorError::rejected("step.logs_too_large"));
    }
    let bytes = serde_json::to_vec(&logs).map_or(usize::MAX, |b| b.len());
    if bytes > MAX_REPORT_LOG_JSON_BYTES {
        return Err(ActorError::Rejected {
            code: "request.value_too_large",
            errors: vec![runbook_model::FieldError {
                path: "/logs".into(),
                code: "request.value_too_large".into(),
                message: format!(
                    "at most {MAX_REPORT_LOG_JSON_BYTES} bytes after secret redaction"
                ),
            }],
        });
    }
    Ok(logs)
}

/// CF12c: whether a report carrying `lines` as its logs keeps them within
/// [`report_logs`]'s bounds once redacted and split, as the server checks
/// them. A client whose lines fit a report as sent (1 MiB as JSON) but not
/// once redacted (a marker is longer than the short secret it replaces)
/// sends them through the logs route instead, which counts only the
/// step's 5 MiB, so the same file is accepted whichever way it goes.
pub fn report_logs_fit(lines: &[String]) -> bool {
    report_logs(lines.to_vec()).is_ok()
}

/// CF12b review round 2: whether `step_run_id` has a non-blank log line
/// appended through the logs route (one that is not empty or whitespace,
/// as the engine judges a report's own lines). Every store answers through
/// its [`ClaimStore::read_logs`] and this one predicate, so all count
/// alike, Cloudflare's lines moved to R2 included. Pages grow from 64 lines
/// (the usual case reads one) to 65,536, so a step of blank lines costs a
/// few reads; its 5 MiB bounds the whole scan.
pub async fn logs_appended(
    store: &impl ClaimStore,
    id: &ExecutionId,
    step_run_id: &StepRunId,
) -> Result<bool, StoreError> {
    let (mut after, mut limit) = (0u64, 64usize);
    loop {
        let page = store.read_logs(id, step_run_id, after, limit).await?;
        if page.iter().any(|(_, line)| !line.trim().is_empty()) {
            return Ok(true);
        }
        match page.last() {
            Some((seq, _)) if page.len() == limit => after = *seq,
            _ => return Ok(false),
        }
        limit = (limit * 8).min(65_536);
    }
}

/// CF12b review round 2: whether `request` can be accepted only with lines
/// appended through the logs route: it reports success on a task that
/// requires `log` evidence, and neither it nor an earlier report carries a
/// non-blank line. Only then does the core ask the store
/// ([`logs_appended`]).
pub fn needs_appended_logs<S: SessionStore>(work: &ActorWork<S>, request: &ReportRequest) -> bool {
    use runbook_spec::ir::{EvidenceKind, NodeKind};
    let state = &work.runtime.state;
    let Some(step) = state
        .steps
        .iter()
        .find(|s| s.id.as_ref() == Some(&request.step_run_id))
    else {
        return false;
    };
    let requires = state.node(&step.step_id).is_some_and(|node| {
        matches!(&node.kind, NodeKind::Task(task)
            if task.evidence.required.contains(&EvidenceKind::Log))
    });
    requires
        && request.status == runbook_model::ReportStatus::Succeeded
        && !step
            .logs
            .iter()
            .chain(&request.logs)
            .any(|line| !line.trim().is_empty())
}

/// Lease-only claim writes (heartbeat, logs) that carry no engine event,
/// plus the current claim read. All are fenced by generation and owner.
pub trait ClaimStore {
    fn heartbeat(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        replay: Option<&crate::actor::Replay>,
    ) -> impl Future<Output = Result<Option<i64>, StoreError>>;
    fn append_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        lines: Vec<String>,
        replay: Option<&crate::actor::Replay>,
    ) -> impl Future<Output = Result<u64, StoreError>>;
    fn claim_record(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> impl Future<Output = Result<Option<ClaimWrite>, StoreError>>;
    /// F46/D33: up to `limit` persisted lines with sequence above `after`,
    /// in order, as `(seq, line)`; sequences start at 1. A run outside this
    /// store's scope is `NotFound`; a step without logs reads empty.
    fn read_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<(u64, String)>, StoreError>>;
}

/// The current claim for a step, verified against this principal and ref.
/// Reserved for session operations; heartbeat/logs fence in the store.
pub(crate) fn fence<S: SessionStore>(
    work: &ActorWork<S>,
    principal: &ActorPrincipal,
    step_run_id: &StepRunId,
    claim: &ClaimRef,
) -> Result<ClaimWrite, ActorError> {
    let current = work
        .snapshot
        .claims
        .get(step_run_id)
        .ok_or(ActorError::rejected("claim.not_found"))?;
    if current.released
        || current.claim_id != claim.id
        || current.generation != claim.generation.get()
        || current.owner != principal.actor
        || current
            .lease_deadline_at_ms
            .is_some_and(|deadline| work.at_ms() >= deadline)
    {
        return Err(ActorError::rejected("claim.stale_generation"));
    }
    Ok(current.clone())
}

fn lease_deadline(principal: &ActorPrincipal, now: i64) -> Option<i64> {
    match principal.kind() {
        Some(ActorKind::Agent) => Some(now + AGENT_LEASE_MS),
        _ => None,
    }
}

pub async fn claim_step<S: SessionStore>(
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    step_run_id: &StepRunId,
    request: &ClaimRequest,
    claim_id: ClaimId,
    replay: Option<&crate::actor::Replay>,
) -> Result<ClaimRef, ActorError> {
    let operation = format!("claim_step:{}", step_run_id);
    if let Some(saved) = work
        .replayed::<ClaimRef>(principal, replay, &operation)
        .await?
    {
        return Ok(saved);
    }
    // PRD §6: any pause stops new claims, a manual pause and an
    // approval-expiration pause alike (F15).
    if !work.runtime.state.pause_reasons.is_empty() {
        return Err(ActorError::rejected("execution.paused"));
    }
    let index = work.step_index(step_run_id)?;
    let step = &work.runtime.state.steps[index];
    let node = work
        .runtime
        .state
        .node(&step.step_id)
        .ok_or(ActorError::rejected("step.not_found"))?;
    let runbook_spec::ir::NodeKind::Task(task) = &node.kind else {
        return Err(ActorError::rejected("step.not_task"));
    };
    let declared = request
        .actor
        .as_ref()
        .map(|declaration| declaration.kind)
        .or_else(|| principal.kind());
    // After a handoff the task continues with an eligible human only.
    let handoff = step
        .human_continuation
        .then_some(step.handoff.as_ref())
        .flatten();
    // LANG7: a person claims an agent task only by taking it over, and only
    // while it is ready; a live agent claim is never taken.
    let take_over = request.take_over;
    if take_over {
        if declared != Some(ActorKind::Human)
            || principal.kind() != Some(ActorKind::Human)
            || task.actor != runbook_spec::ir::Actor::Agent
            || step.human_continuation
        {
            return Err(ActorError::rejected("claim.take_over_not_allowed"));
        }
        if step.status == StepStatus::Claimed {
            return Err(ActorError::rejected("claim.take_over_live_claim"));
        }
    }
    let kind_allowed = match task.actor {
        _ if step.human_continuation => declared == Some(ActorKind::Human),
        runbook_spec::ir::Actor::Any => true,
        runbook_spec::ir::Actor::Human => declared == Some(ActorKind::Human),
        runbook_spec::ir::Actor::Agent => {
            declared == Some(ActorKind::Agent) || (take_over && declared == Some(ActorKind::Human))
        }
    };
    if !kind_allowed || declared != principal.kind() {
        return Err(ActorError::rejected("claim.actor_mismatch"));
    }
    let mut reresolved = None;
    if let Some(handoff) = handoff {
        let eligible = principal.user_id().is_some_and(|user| {
            crate::identity::vote_eligibility(
                &work.runtime.directory,
                &work.runtime.scope.project_id,
                user,
                &handoff.eligible_recipients,
            )
            .eligible
        });
        if !eligible {
            return Err(ActorError::rejected("auth.forbidden"));
        }
    } else if task.assignment.is_strict()
        && (task.actor != runbook_spec::ir::Actor::Agent || declared == Some(ActorKind::Human))
    {
        // LANG7: on an `actor: any` or `actor: human` task only the strict
        // assignee, a person, may claim (an agent has no user, so it is
        // refused); on an `actor: agent` task `actor` governs agents and the
        // assignee binds only the person taking it over. The resolution
        // recorded for this attempt names the assignee; the claimant must
        // match it in the live directory, with run access. One that resolved
        // to nobody is resolved again against the live directory, never
        // more broadly, and recorded when it now names the claimant.
        let directory = &work.runtime.directory;
        let project = &work.runtime.scope.project_id;
        let recorded = step
            .strict_assignee
            .as_ref()
            .filter(|resolved| resolved.request.attempt == step.attempt);
        let user = principal.user_id();
        let allowed = match (recorded, user) {
            (Some(resolved), Some(user)) if !resolved.unresolved() => {
                crate::identity::strict_assignee_allows(directory, project, user, resolved)
            }
            (Some(resolved), Some(user)) => {
                let live = crate::identity::assignee_recipients(
                    directory,
                    project,
                    runbook_engine::AssigneeRequest {
                        base_sequence: work.runtime.state.sequence,
                        ..resolved.request.clone()
                    },
                    work.runtime.state.compiled().users(),
                );
                let found = !live.unresolved()
                    && crate::identity::strict_assignee_allows(directory, project, user, &live);
                if found {
                    reresolved = Some(live);
                }
                found
            }
            _ => false,
        };
        // LANG9: once a step escalates, its escalation recipients may claim
        // it too (checked against the live directory, with run access).
        let escalated = user.is_some_and(|user| {
            step.followup_escalation.as_ref().is_some_and(|e| {
                crate::identity::vote_eligibility(directory, project, user, &e.recipients).eligible
            })
        });
        if !allowed && !escalated {
            return Err(ActorError::rejected("claim.not_assignee"));
        }
    }
    if work.runtime.state.steps[index].status != StepStatus::Ready {
        return Err(ActorError::rejected("step.not_claimable"));
    }
    let expected = work
        .snapshot
        .claims
        .get(step_run_id)
        .map_or(0, |claim| claim.generation);
    let generation = expected + 1;
    if let Some(resolution) = reresolved {
        work.append(
            Event::StepAssigneeResolved {
                resolution: Box::new(resolution),
            },
            ActorRef::System,
        )?;
    }
    work.append(
        Event::StepClaimed {
            step_run_id: step_run_id.clone(),
        },
        principal.actor.clone(),
    )?;
    if take_over {
        work.append(
            Event::StepTakenOver {
                step_run_id: step_run_id.clone(),
                previous_actor_kind: ActorKind::Agent,
            },
            principal.actor.clone(),
        )?;
    }
    work.settle()?;
    let write = ClaimWrite {
        execution_id: work.runtime.state.id.clone(),
        step_run_id: step_run_id.clone(),
        expected_generation: expected,
        claim_id: claim_id.clone(),
        generation,
        action: ClaimAction::Claim,
        owner: principal.actor.clone(),
        session: principal.session(),
        lease_deadline_at_ms: lease_deadline(principal, work.at_ms()),
        released: false,
    };
    let response = ClaimRef {
        id: claim_id,
        generation: generation.try_into().expect("claim generation is nonzero"),
    };
    match work
        .commit_replay(vec![write], replay, &operation, &response)
        .await?
    {
        crate::CommitResult::Committed(_) => Ok(response),
        crate::CommitResult::Conflict => Err(ActorError::rejected("claim.conflict")),
    }
}

pub async fn release_claim<S: SessionStore>(
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    step_run_id: &StepRunId,
    claim: &ClaimRef,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), ActorError> {
    let operation = format!("release_claim:{}", step_run_id);
    if let Some(saved) = work.replayed::<()>(principal, replay, &operation).await? {
        return Ok(saved);
    }
    let current = fence(&work, principal, step_run_id, claim)?;
    work.append(
        Event::StepClaimReleased {
            step_run_id: step_run_id.clone(),
            reason: None,
        },
        principal.actor.clone(),
    )?;
    work.settle()?;
    let write = ClaimWrite {
        action: ClaimAction::Release,
        expected_generation: current.generation,
        released: true,
        ..current
    };
    match work
        .commit_replay(vec![write], replay, &operation, &())
        .await?
    {
        crate::CommitResult::Committed(_) => Ok(()),
        crate::CommitResult::Conflict => Err(ActorError::rejected("claim.conflict")),
    }
}

/// Lease expiry, driven by the claim-lease work item: back to ready, so
/// `lease_expired` work becomes available again through settle.
pub async fn expire_claim<S: SessionStore>(
    mut work: ActorWork<S>,
    step_run_id: &StepRunId,
    generation: u64,
) -> Result<bool, ActorError> {
    let Some(current) = work.snapshot.claims.get(step_run_id).cloned() else {
        return Ok(false);
    };
    if current.released
        || current.generation != generation
        || current
            .lease_deadline_at_ms
            .is_none_or(|deadline| work.at_ms() < deadline)
    {
        return Ok(false);
    }
    let index = work.step_index(step_run_id)?;
    // A claim stranded by an older commit that moved its step out of
    // `Claimed` without a claim write (F6) is released without an event:
    // the step has nothing to expire, and the lease must stop being offered.
    if work.runtime.state.steps[index].status == StepStatus::Claimed {
        work.append(
            Event::StepClaimExpired {
                step_run_id: step_run_id.clone(),
            },
            ActorRef::System,
        )?;
        work.settle()?;
    }
    let write = ClaimWrite {
        action: ClaimAction::Expire,
        expected_generation: current.generation,
        released: true,
        ..current
    };
    match work.commit(vec![write]).await? {
        crate::CommitResult::Committed(_) => Ok(true),
        crate::CommitResult::Conflict => Ok(false),
    }
}

pub async fn report_step<S: SessionStore>(
    work: ActorWork<S>,
    principal: &ActorPrincipal,
    request: &ReportRequest,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), ActorError> {
    report_step_with_files(work, principal, request, vec![], replay).await
}

/// UP1: a report whose `evd_` evidence entries the host resolved to
/// finalized files (`uploads::report_files`, or `uploads::report_with_uploads`
/// which does both). The event records the files with their verified types.
pub async fn report_step_with_files<S: SessionStore>(
    work: ActorWork<S>,
    principal: &ActorPrincipal,
    request: &ReportRequest,
    files: Vec<runbook_model::EvidenceFileRef>,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), ActorError> {
    report_step_logged(work, principal, request, files, false, replay).await
}

/// CF12b review round 2: as [`report_step_with_files`], where
/// `appended_logs` says whether the step run has a non-blank line appended
/// through the logs route ([`logs_appended`]); such lines satisfy required
/// `log` evidence, and the event records it.
pub async fn report_step_logged<S: SessionStore>(
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    request: &ReportRequest,
    files: Vec<runbook_model::EvidenceFileRef>,
    appended_logs: bool,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), ActorError> {
    let operation = format!("report_step:{}", request.step_run_id);
    if let Some(saved) = work.replayed::<()>(principal, replay, &operation).await? {
        return Ok(saved);
    }
    let current = fence(&work, principal, &request.step_run_id, &request.claim)?;
    // PRD §17 per-step limits; the stored report is reloaded with every
    // later operation on the run (F9).
    let output_bytes = serde_json::to_vec(&request.outputs)
        .map_err(|_| ActorError::rejected("step.invalid_outputs"))?
        .len();
    if output_bytes > MAX_STEP_OUTPUT_BYTES {
        return Err(ActorError::rejected("step.outputs_too_large"));
    }
    let index = work.step_index(&request.step_run_id)?;
    let node = work
        .runtime
        .state
        .node(&work.runtime.state.steps[index].step_id)
        .ok_or(ActorError::rejected("step.not_found"))?
        .clone();
    let record = work.runtime.state.steps[index].clone();
    let mut request = request.clone();
    request.logs = report_logs(request.logs)?;
    let validated =
        runbook_engine::validate_report_with_logs(&node, &request, &record, files, appended_logs)
            .map_err(|error| ActorError::Rejected {
            code: error.code(),
            errors: error.errors().to_vec(),
        })?;
    work.append(validated.into_event(), principal.actor.clone())?;
    work.settle()?;
    let write = ClaimWrite {
        action: ClaimAction::Release,
        expected_generation: current.generation,
        released: true,
        ..current
    };
    match work
        .commit_replay(vec![write], replay, &operation, &())
        .await?
    {
        crate::CommitResult::Committed(_) => Ok(()),
        crate::CommitResult::Conflict => Err(ActorError::rejected("claim.conflict")),
    }
}

pub async fn update_checklist<S: SessionStore>(
    mut work: ActorWork<S>,
    principal: &ActorPrincipal,
    step_run_id: &StepRunId,
    request: &ChecklistRequest,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), ActorError> {
    let operation = format!("update_checklist:{}", step_run_id);
    if let Some(saved) = work.replayed::<()>(principal, replay, &operation).await? {
        return Ok(saved);
    }
    fence(&work, principal, step_run_id, &request.claim)?;
    let ticks: Vec<bool> = request
        .checklist
        .iter()
        .map(|tick| tick.completed)
        .collect();
    let event = runbook_engine::update_checklist(&work.runtime.state, step_run_id, &ticks)?;
    work.append(event, principal.actor.clone())?;
    work.settle()?;
    match work.commit_replay(vec![], replay, &operation, &()).await? {
        crate::CommitResult::Committed(_) => Ok(()),
        crate::CommitResult::Conflict => Err(ActorError::rejected("claim.conflict")),
    }
}

pub async fn heartbeat(
    store: &impl ClaimStore,
    principal: &ActorPrincipal,
    id: &ExecutionId,
    step_run_id: &StepRunId,
    request: &runbook_model::HeartbeatRequest,
    replay: Option<&crate::actor::Replay>,
) -> Result<Option<i64>, ActorError> {
    if replay.is_some_and(|r| r.principal != principal.principal) {
        return Err(ActorError::rejected("auth.forbidden"));
    }
    Ok(store
        .heartbeat(id, step_run_id, &request.claim, &principal.actor, replay)
        .await?)
}

pub async fn append_logs(
    store: &impl ClaimStore,
    principal: &ActorPrincipal,
    id: &ExecutionId,
    step_run_id: &StepRunId,
    request: &LogsRequest,
    replay: Option<&crate::actor::Replay>,
) -> Result<u64, ActorError> {
    if replay.is_some_and(|r| r.principal != principal.principal) {
        return Err(ActorError::rejected("auth.forbidden"));
    }
    Ok(store
        .append_logs(
            id,
            step_run_id,
            &request.claim,
            &principal.actor,
            split_log_lines(crate::redact::redact_lines(request.logs.clone())),
            replay,
        )
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_split_on_character_boundaries() {
        assert_eq!(split_log_line("", 4), vec![""]);
        assert_eq!(split_log_line("abcd", 4), vec!["abcd"]);
        assert_eq!(split_log_line("abcdef", 4), vec!["abcd", "ef"]);
        // "é" is two bytes: a cut at 5 would split one.
        assert_eq!(split_log_line("aééé", 5), vec!["aéé", "é"]);
        assert_eq!(split_log_line("🙂🙂", 1), vec!["🙂", "🙂"]);
    }

    /// CF12b review round 1 (finding 7): the API holds a report's logs to
    /// 1 MiB as sent; redaction could make them about three times that
    /// (each `token=x` becomes `token=[REDACTED:token]`), past what the
    /// report's event row holds. Such a report is refused, and a long line
    /// is kept as entries of at most `MAX_LOG_ENTRY_BYTES`.
    #[test]
    fn a_reports_logs_are_bounded_after_redaction() {
        let long = "a".repeat(MAX_LOG_ENTRY_BYTES + 10);
        let kept = report_logs(vec![long.clone(), "b".into()]).unwrap();
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[..2].concat(), long);
        let secrets = vec!["token=x ".repeat(8_000); 16];
        assert!(serde_json::to_vec(&secrets).unwrap().len() <= 1_048_576);
        let refused = report_logs(secrets).unwrap_err();
        assert_eq!(refused.code(), "request.value_too_large");
        let ActorError::Rejected { errors, .. } = refused else {
            panic!("a rejection");
        };
        assert_eq!(errors[0].path, "/logs");
    }

    /// CF12c: the client's question, answered as the server checks: such
    /// logs do not fit a report (they go through the logs route); ordinary
    /// ones and a long line, kept as several entries, do.
    #[test]
    fn report_logs_fit_answers_as_the_report_is_checked() {
        assert!(!report_logs_fit(&vec!["token=x ".repeat(8_000); 16]));
        assert!(report_logs_fit(&["a".repeat(MAX_LOG_ENTRY_BYTES + 10)]));
        assert!(report_logs_fit(&["password=hunter2".into(), "ok".into()]));
        assert!(report_logs_fit(&[]));
    }
}
