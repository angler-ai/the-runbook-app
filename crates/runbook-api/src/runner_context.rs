//! Claim-fenced context for the worker's built-in tools.
use crate::{
    Request,
    actor_requests::post_identity,
    backends::Backends,
    executions::{ExecutionApiError, restore_state},
    grants,
    runners::{Error, storage},
};
use runbook_core::{
    ClaimWrite, ExecutionStore,
    actor::{ActorPrincipal, Replay},
    catalog::{CatalogStore, RunSummary, TenantScope},
    claims::ClaimStore,
    directory::DirectoryStore,
    grants::runner_key,
    memory_entries::{self as memory, MemoryQuery},
    runners::{RunnerPrincipal, RunnerStore},
    search::RunFilters,
    start::{StartRecord, StartStore},
};
use runbook_engine::{ExecutionState, StepState};
use runbook_model::*;
use serde_json::{Value, json};

const FORBIDDEN: Error = (403, "auth.forbidden");
const NOT_FOUND: Error = (404, "resource.not_found");
const BAD_QUERY: Error = (400, "request.invalid_query");
const RESPONSE_BYTES: usize = 48 * 1024;
const PAGE_SIZE: usize = 10;

fn api(error: ExecutionApiError) -> Error {
    (error.status, error.code)
}

fn bounded(mut value: Value) -> Value {
    if value.to_string().len() <= RESPONSE_BYTES {
        return value;
    }
    // Truncate by complete entries, preserving the orientation and the
    // shapes of the collections the model reads.
    value["truncated"] = json!(true);
    for key in ["inputs", "outputs", "summary"] {
        if value
            .get(key)
            .is_some_and(|part| part.to_string().len() > RESPONSE_BYTES / 4)
        {
            value[key] = Value::Null;
        }
    }
    let mut remaining = RESPONSE_BYTES - 32;
    let keys = ["steps", "items", "logs", "evidence", "files"];
    let mut lists = keys.map(|key| value.get_mut(key).map(Value::take));
    remaining = remaining.saturating_sub(value.to_string().len());
    for (key, list) in keys.into_iter().zip(&mut lists) {
        if let Some(Value::Array(items)) = list {
            items.retain(|item| {
                let bytes = item.to_string().len() + 1;
                if bytes >= remaining {
                    return false;
                }
                remaining -= bytes;
                true
            });
            value[key] = json!(items);
        }
    }
    if value.to_string().len() > RESPONSE_BYTES {
        return json!({"truncated": true});
    }
    value
}

fn step_summary(step: &StepState) -> Value {
    json!({
        "id": step.id, "path": step.step_id, "status": step.status,
        "outputs": step.outputs, "summary": step.summary, "verdict": step.verdict,
        "approval": step.approval.as_ref().map(|a| json!({
            "requested": a.requested.is_some(), "decision": a.decision,
            "automatic_decision": a.automatic_decision, "agent_decision": a.agent_decision,
            "expired": a.expired,
        })),
    })
}

fn overview(
    state: &ExecutionState,
    record: &StartRecord,
    scope: &TenantScope,
    project: &str,
    runbook: &str,
    summary: &RunSummary,
) -> Value {
    let start_reason = if record.context.parent.is_some() {
        "parent_step"
    } else if record.rerun_of.is_some() {
        "rerun"
    } else {
        match record.context.created_by.split(':').next() {
            Some("schedule") => "schedule",
            Some("webhook") => "webhook",
            Some("service") => "service_request",
            Some("agent") => "agent_request",
            _ => "direct_request",
        }
    };
    json!({
        "id": state.id, "title": orientation_label(&state.title), "start_reason": start_reason, "status": state.status,
        "project": project, "project_id": scope.project_id,
        "runbook_id": record.runbook_id, "runbook_slug": runbook,
        "runbook_version": record.version, "runbook_version_id": record.version_id,
        "created_by": record.context.created_by, "created_at_ms": summary.created_at_ms,
        "parent": record.context.parent, "rerun_of": record.rerun_of,
        "inputs": state.inputs,
        "steps": state.steps.iter().filter(|s| s.id.is_some()).map(step_summary).collect::<Vec<_>>(),
    })
}

fn holds_context(claim: &ClaimWrite, key: &str, binding: &ClaimRef, now: i64) -> bool {
    !claim.released
        && claim.session.principal == key
        && claim.claim_id == binding.id
        && claim.generation == binding.generation.get()
        && claim
            .lease_deadline_at_ms
            .is_some_and(|deadline| deadline > now)
}

fn orientation_label(value: &str) -> &str {
    let mut end = value.len().min(1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(crate) async fn handle<B: Backends>(
    backends: &B,
    request: &Request,
    runner: &RunnerPrincipal,
    step: &str,
) -> Result<(u16, Value), Error> {
    let key = runner_key(runner.runner_id.as_ref().ok_or(FORBIDDEN)?);
    let (sr, scope, execution) = grants::step_scope(backends, runner, step).await?;
    let (store, work) = grants::work(backends, &scope, &execution).await?;
    let claim_id: ClaimId = request
        .query
        .get("claim_id")
        .ok_or(FORBIDDEN)?
        .parse()
        .map_err(|_| FORBIDDEN)?;
    let generation = request
        .query
        .get("generation")
        .ok_or(FORBIDDEN)?
        .parse()
        .map_err(|_| FORBIDDEN)?;
    let binding = ClaimRef {
        id: claim_id,
        generation,
    };
    let claim = work
        .snapshot
        .claims
        .get(&sr)
        .filter(|claim| holds_context(claim, &key, &binding, work.at_ms()))
        .ok_or(FORBIDDEN)?;
    let index = work.step_index(&sr).map_err(|_| FORBIDDEN)?;
    let state = &work.runtime.state;
    let current = &state.steps[index];
    let decision = runbook_engine::decision_open(state, current);
    if current.status != StepStatus::Claimed && !decision {
        return Err(FORBIDDEN);
    }
    if decision && request.method == "POST" {
        return Err(FORBIDDEN);
    }
    let action = request.path.rsplit('/').next().ok_or(NOT_FOUND)?;
    let text = request
        .query
        .get("q")
        .filter(|q| !q.trim().is_empty())
        .cloned();
    if text.as_ref().is_some_and(|q| q.len() > 512) {
        return Err(BAD_QUERY);
    }
    let runbook_id = work.record.runbook_id.clone();
    let runbook_scope = MemoryScope::Runbook {
        runbook_id: runbook_id.clone(),
    };
    let value = match (request.method.as_str(), action) {
        ("GET", "memory") => {
            let query = MemoryQuery {
                within: vec![runbook_scope, MemoryScope::Project],
                text,
                status: Some(MemoryStatus::Active),
                ..MemoryQuery::default()
            };
            let page = memory::search(&store, &scope, &query, None, PAGE_SIZE)
                .await
                .map_err(storage)?;
            json!({"items": page.items.iter().map(memory::view).collect::<Vec<_>>()})
        }
        ("POST", "memory") => {
            let proposal: RunnerMemoryProposal = crate::runners::decode(request)?;
            let principal = ActorPrincipal {
                principal: key,
                actor: claim.owner.clone(),
                credential_ref: claim.session.credential_ref.clone(),
            };
            let post = post_identity(request, &principal.principal).map_err(api)?;
            let id =
                memory::memory_id_for(&scope, &format!("{}:{sr}", principal.principal), &post.key);
            let replay = Replay {
                key: post.key,
                principal: post.principal,
                request_hash: post.request_hash,
            };
            let body = SaveMemoryRequest {
                title: proposal.title,
                body_markdown: proposal.body_markdown,
                tags: proposal.tags,
                step_id: Some(memory::step_key(&current.step_id)),
                expires_at: None,
                propose: true,
            };
            let entry = memory::propose_from_runner(work, &principal, id, &body, &replay, &sr)
                .await
                .map_err(|e| api(crate::memory::rejection(e)))?;
            return Ok((201, json!({"id": entry.id, "status": entry.status})));
        }
        ("GET", "step") => {
            let id = request.query.get("step_run_id").ok_or(BAD_QUERY)?;
            let prior = state
                .steps
                .iter()
                .find(|s| {
                    s.id.as_ref()
                        .is_some_and(|candidate| candidate.as_str() == id && candidate != &sr)
                        && s.finished_at_ms.is_some()
                })
                .ok_or(NOT_FOUND)?;
            let mut value = step_summary(prior);
            let lines = store
                .read_logs(&execution, prior.id.as_ref().ok_or(NOT_FOUND)?, 0, 21)
                .await
                .map_err(storage)?;
            value["logs_truncated"] = json!(lines.len() > 20);
            value["logs"] = json!(
                lines
                    .into_iter()
                    .take(20)
                    .map(|(seq, line)| json!({"seq": seq, "line": line}))
                    .collect::<Vec<_>>()
            );
            value["evidence"] = json!(prior.evidence);
            value["files"] = json!(prior.files);
            value
        }
        ("GET", "runs" | "run" | "past-run") => {
            let current_summary = store
                .get_run(&execution)
                .await
                .map_err(storage)?
                .ok_or(NOT_FOUND)?;
            let earlier = |candidate: &RunSummary| {
                candidate.runbook_id == runbook_id
                    && (candidate.created_at_ms, &candidate.execution_id)
                        < (current_summary.created_at_ms, &execution)
            };
            if action == "runs" {
                let filters = RunFilters {
                    runbook_id: Some(runbook_id.clone()),
                    text,
                    created_to_ms: Some(current_summary.created_at_ms.saturating_add(1)),
                    ..RunFilters::default()
                };
                let page = store
                    .search_runs(filters, None, PAGE_SIZE + 1)
                    .await
                    .map_err(storage)?;
                let items: Vec<_> = page
                    .runs
                    .iter()
                    .filter(|r| earlier(r))
                    .take(PAGE_SIZE)
                    .map(|r| {
                        json!({
                            "id": r.execution_id, "title": r.title, "status": r.status,
                            "created_at_ms": r.created_at_ms, "completed_at_ms": r.completed_at_ms,
                        })
                    })
                    .collect();
                return Ok((200, bounded(json!({"items": items}))));
            }
            let runbook = store
                .read_runbook(&runbook_id)
                .await
                .map_err(storage)?
                .ok_or(NOT_FOUND)?;
            let directory = backends
                .directory()
                .snapshot(&scope.org_id)
                .await
                .map_err(storage)?;
            let project = directory.projects.get(&scope.project_id).ok_or(NOT_FOUND)?;
            if action == "run" {
                let mut view = overview(
                    state,
                    &work.record,
                    &scope,
                    &project.slug,
                    &runbook.slug,
                    &current_summary,
                );
                view["step"] = json!({
                    "id": current.step_id,
                    "title": runbook_engine::step_title(state, current).as_deref().map(orientation_label),
                });
                if let Some(attempt) = current.attempt_history.last() {
                    view["previous_attempt"] = json!({
                        "attempt": attempt.attempt,
                        "status": attempt.report.status,
                        "summary": attempt.report.summary,
                        "verdict": attempt.report.verdict,
                        "timed_out": attempt.timed_out,
                        "logs": attempt.report.logs.iter().take(8).map(|line| line.chars().take(512).collect::<String>()).collect::<Vec<_>>(),
                    });
                }
                view
            } else {
                let past: ExecutionId = request
                    .query
                    .get("run_id")
                    .ok_or(BAD_QUERY)?
                    .parse()
                    .map_err(|_| NOT_FOUND)?;
                let past_summary = store
                    .get_run(&past)
                    .await
                    .map_err(storage)?
                    .filter(earlier)
                    .ok_or(NOT_FOUND)?;
                let record = store
                    .start_record(&past)
                    .await
                    .map_err(storage)?
                    .filter(|record| record.runbook_id == runbook_id)
                    .ok_or(NOT_FOUND)?;
                let run = store
                    .load(&past)
                    .await
                    .map_err(|e| api(crate::executions::store_error(e)))?;
                let state = restore_state(&store, &record, &run).await.map_err(api)?;
                overview(
                    &state,
                    &record,
                    &scope,
                    &project.slug,
                    &runbook.slug,
                    &past_summary,
                )
            }
        }
        _ => return Err(NOT_FOUND),
    };
    Ok((200, bounded(value)))
}

/// Resolve a name when runner-api saves an agent version. Reading the
/// body by the selected hash keeps it consistent with that exact version.
pub(crate) async fn skill<B: Backends>(
    backends: &B,
    runner: &RunnerPrincipal,
    project: &str,
    name: &str,
) -> Result<(u16, Value), Error> {
    let project_id: ProjectId = project.parse().map_err(|_| FORBIDDEN)?;
    let install = backends
        .runners()
        .status(&runner.org_id, backends.wall_clock_ms())
        .await
        .map_err(crate::runners::runner_error)?;
    if install.install_id.as_ref() != Some(&runner.install_id)
        || !install
            .groups
            .iter()
            .any(|group| group.projects.contains(&project_id))
    {
        return Err(FORBIDDEN);
    }
    let store = backends.execution(TenantScope {
        org_id: runner.org_id.clone(),
        project_id,
    });
    let skill = store
        .latest_skills()
        .await
        .map_err(storage)?
        .into_iter()
        .find(|skill| skill.name == name)
        .ok_or(NOT_FOUND)?;
    let main = skill
        .manifest
        .entries
        .iter()
        .find(|file| file.path == "SKILL.md")
        .ok_or(NOT_FOUND)?;
    if main.size > 16 * 1024 {
        return Err((422, "skill.too_large"));
    }
    let bytes =
        runbook_core::skill_library::read_pinned_file(&store, &store, &skill.hash, "SKILL.md")
            .await
            .map_err(storage)?;
    let pin = RunnerSkillPin {
        hash: runbook_core::authoring::hex(&skill.hash),
        name: skill.name,
        body: String::from_utf8(bytes).map_err(storage)?,
    };
    let value = serde_json::to_value(pin).map_err(storage)?;
    if value.to_string().len() > 16 * 1024 {
        return Err((422, "skill.too_large"));
    }
    Ok((200, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use runbook_core::{ClaimAction, ClaimSession};

    #[test]
    fn expired_claim_is_refused_before_claim_cleanup() {
        let binding = ClaimRef {
            id: "clm_00000000000000000000000001".parse().unwrap(),
            generation: 1.try_into().unwrap(),
        };
        let claim = ClaimWrite {
            execution_id: "exe_00000000000000000000000001".parse().unwrap(),
            step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
            expected_generation: 0,
            claim_id: binding.id.clone(),
            generation: 1,
            owner: ActorRef::System,
            session: ClaimSession {
                principal: "runner:r".into(),
                actor: ActorRef::System,
                credential_ref: "runner:r".into(),
            },
            lease_deadline_at_ms: Some(200),
            action: ClaimAction::Claim,
            released: false,
        };
        assert!(holds_context(&claim, "runner:r", &binding, 199));
        assert!(!holds_context(&claim, "runner:r", &binding, 200));
        assert!(!holds_context(&claim, "runner:r", &binding, 201));
    }
}
