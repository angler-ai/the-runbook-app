//! R0-4: what runner tokens do to runs. The claim token claims and works
//! its group's agents' steps (claim, heartbeat, logs, report, release, the
//! claimed step read); the release token asks for grants bound to a live
//! claim and records revocations; the directory token reports waiting
//! reasons, runner events (org webhooks) and lists ready work.
use crate::{
    Request,
    actor_requests::{ActorRequest, decode, post_identity, validate_declaration},
    actor_store::{Identity, actor_error},
    actors::ActorTarget,
    backends::{Backends, org_store},
    executions::ExecutionApiError as ApiError,
    runners::{Error, runner_error},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_core::{
    actor::{ActorWork, Replay, begin_actor},
    agent_directory::AgentDirectory,
    catalog::TenantScope,
    directory::DirectoryStore,
    grants::{GrantContext, runner_key},
    runners::{HEARTBEAT_STALE_MS, RunnerPrincipal, RunnerStore},
    step_lookup::StepRunLookup,
};
use runbook_model::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// `/v1/step-runs/{sr}/grants*`.
pub fn recognizes(path: &str) -> bool {
    path.starts_with("/v1/step-runs/")
        && (path.ends_with("/grants")
            || path.ends_with("/grants/revoked")
            || path.contains("/context/"))
}

fn api(e: ApiError) -> Error {
    (e.status, e.code)
}

/// The run a step belongs to, in the runner's org.
pub(crate) async fn step_scope<B: Backends>(
    backends: &B,
    runner: &RunnerPrincipal,
    sr: &str,
) -> Result<(StepRunId, TenantScope, ExecutionId), Error> {
    let sr: StepRunId = sr.parse().map_err(|_| (404, "resource.not_found"))?;
    let (execution, project) = org_store(backends, &runner.org_id)
        .step_run_execution(&runner.org_id, &sr)
        .await
        .map_err(crate::runners::storage)?
        .ok_or((404, "resource.not_found"))?;
    let scope = TenantScope {
        org_id: runner.org_id.clone(),
        project_id: project,
    };
    backends.label_execution_trace(&scope, &execution, &sr);
    Ok((sr, scope, execution))
}

pub(crate) async fn work<B: Backends>(
    backends: &B,
    scope: &TenantScope,
    execution: &ExecutionId,
) -> Result<(B::Execution, ActorWork<B::Execution>), Error> {
    let store = backends.execution(scope.clone());
    let work = begin_actor(&store, &backends.directory(), scope.clone(), execution)
        .await
        .map_err(|e| api(actor_error(e)))?;
    Ok((store, work))
}

/// The directory entry of the agent a step targets, if it is one of this
/// install's.
async fn entry_of<B: Backends>(
    backends: &B,
    runner: &RunnerPrincipal,
    agent: Option<String>,
) -> Result<Option<AgentDirectoryEntry>, Error> {
    let Some(agent) = agent else { return Ok(None) };
    Ok(backends
        .runners()
        .agent_by_id(&runner.org_id, &agent)
        .await
        .map_err(runner_error)?
        .filter(|(install, _)| *install == runner.install_id)
        .map(|(_, entry)| entry))
}

/// Claim-token step operations: the claim with the runner checks, then the
/// usual actor flow under the runner's own principal key.
pub(crate) async fn step_operation<B: Backends>(
    backends: &B,
    r: &Request,
    runner: &RunnerPrincipal,
    params: &BTreeMap<String, String>,
    op: &'static str,
) -> Result<(u16, Value), Error> {
    let runner_id = runner.runner_id.clone().ok_or((403, "auth.forbidden"))?;
    let (sr, scope, execution) = step_scope(backends, runner, &params["sr"]).await?;
    let (store, work) = work(backends, &scope, &execution).await?;
    let identity = Identity {
        service_attribution: None,
        key: runner_key(&runner_id),
        human: None,
        credential_ref: runner_key(&runner_id),
    };
    if matches!(op, "step" | "skill") {
        let claim = work
            .snapshot
            .claims
            .get(&sr)
            .ok_or((403, "auth.forbidden"))?;
        if claim.released
            || claim.session.principal != identity.key
            || claim
                .lease_deadline_at_ms
                .is_some_and(|t| t <= work.at_ms())
        {
            return Err((403, "auth.forbidden"));
        }
        let body = crate::execution_access::claimed_read::<B>(&store, work, &sr, op, r)
            .await
            .map_err(api)?;
        return Ok((200, body));
    }
    let replay = post_identity(r, &identity.key).map_err(api)?;
    let target = ActorTarget {
        operation: op,
        scope: scope.clone(),
        execution_id: execution,
        step_run_id: Some(sr.clone()),
        signal_name: None,
    };
    let operation = crate::actor_store::operation(&target, &replay).map_err(api)?;
    let replay = Replay {
        key: replay.key,
        principal: replay.principal,
        request_hash: replay.request_hash,
    };
    if let Some(saved) = work
        .replayed::<Value>(
            &crate::actor_store::control_principal(&identity),
            Some(&replay),
            &operation,
        )
        .await
        .map_err(|e| api(actor_error(e)))?
    {
        return Ok((200, saved));
    }
    let request = decode(op, sr.as_str(), r.body.clone(), &[]).map_err(api)?;
    match request {
        ActorRequest::Claim(claim) => {
            validate_declaration(
                ActorKind::Agent,
                Some(&claim.session_id),
                claim.actor.as_ref(),
                true,
            )
            .map_err(api)?;
            let agent =
                runbook_core::grants::target_agent(&work, &sr).map_err(|e| api(actor_error(e)))?;
            let entry = entry_of(backends, runner, agent).await?;
            let actor = crate::actor_store::declared_principal(
                &identity,
                Some(&claim.session_id),
                claim.actor.as_ref(),
            )
            .map_err(api)?;
            let id: ClaimId = format!("clm_{}", r.new_id)
                .parse()
                .map_err(|_| (422, "request.invalid"))?;
            let decision_supported = backends
                .runners()
                .runner_supports_decisions(
                    &runner.install_id,
                    &runner_id,
                    backends.wall_clock_ms() - HEARTBEAT_STALE_MS,
                )
                .await
                .map_err(runner_error)?;
            let claimed = runbook_core::grants::runner_claim(
                work,
                runner,
                decision_supported,
                entry.as_ref(),
                &actor,
                &sr,
                &claim,
                id,
                Some(&replay),
                backends.trace_id(),
            )
            .await
            .map_err(|e| api(actor_error(e)))?;
            Ok((
                200,
                serde_json::to_value(claimed).map_err(crate::runners::storage)?,
            ))
        }
        request => {
            let value = crate::actor_store::execute_ready::<B>(
                &store,
                work,
                identity,
                target,
                request,
                r.new_id.clone(),
                Some(replay),
            )
            .await
            .map_err(api)?;
            Ok((200, value))
        }
    }
}

/// `POST /v1/step-runs/{sr}/grants` (release token).
pub(crate) async fn issue<B: Backends>(
    backends: &B,
    r: &Request,
    runner: &RunnerPrincipal,
    params: &BTreeMap<String, String>,
) -> Result<(u16, Value), Error> {
    let request: GrantRequest = crate::runners::decode(r)?;
    let (sr, scope, execution) = step_scope(backends, runner, &params["sr"]).await?;
    let store = backends.runners();
    let runner_group = store
        .runner_group(&runner.install_id, &request.runner_id)
        .await
        .map_err(runner_error)?;
    let decision_supported = store
        .runner_supports_decisions(
            &runner.install_id,
            &request.runner_id,
            backends.wall_clock_ms() - HEARTBEAT_STALE_MS,
        )
        .await
        .map_err(runner_error)?;
    let keys = backends.signing_keys().await.map_err(|e| (503, e.code()))?;
    let (_, work) = work(backends, &scope, &execution).await?;
    let agent = runbook_core::grants::target_agent(&work, &sr).map_err(|e| api(actor_error(e)))?;
    let entry = entry_of(backends, runner, agent).await?;
    let mut jti = [0u8; 16];
    backends
        .fill_random(&mut jti)
        .map_err(|_| (503, "server.entropy"))?;
    let issuer = backends.origin();
    let signed = runbook_core::grants::issue(
        work,
        GrantContext {
            runner,
            runner_group,
            decision_supported,
            entry: entry.as_ref(),
            keys: &keys,
            issuer: &issuer,
            jti: jti.iter().map(|b| format!("{b:02x}")).collect(),
        },
        &sr,
        &request,
    )
    .await
    .map_err(|e| api(actor_error(e)))?;
    let view = GrantView {
        grant: signed.grant,
        expires_at: signed.expires_at_ms,
    };
    Ok((
        200,
        serde_json::to_value(view).map_err(crate::runners::storage)?,
    ))
}

/// `POST /v1/step-runs/{sr}/grants/revoked` (release token).
pub(crate) async fn revoked<B: Backends>(
    backends: &B,
    r: &Request,
    runner: &RunnerPrincipal,
    params: &BTreeMap<String, String>,
) -> Result<(u16, Value), Error> {
    let request: GrantRevokedRequest = crate::runners::decode(r)?;
    if !reason_code(&request.reason) || request.credential_id.len() > 128 {
        return Err((422, "request.invalid"));
    }
    let (sr, scope, execution) = step_scope(backends, runner, &params["sr"]).await?;
    let (_, work) = work(backends, &scope, &execution).await?;
    let agent = runbook_core::grants::target_agent(&work, &sr).map_err(|e| api(actor_error(e)))?;
    if entry_of(backends, runner, agent).await?.is_none()
        && !released_to_install(backends, runner, &work, &sr, &request.credential_id).await?
    {
        return Err((403, "grant.not_allowed"));
    }
    runbook_core::grants::revoked(work, &sr, &request.credential_id, &request.reason)
        .await
        .map_err(|e| api(actor_error(e)))?;
    Ok((204, Value::Null))
}

/// A revocation reason is a code (`^[a-z][a-z0-9_.]{0,63}$`, e.g.
/// `grant.claim_moved`), never free text.
fn reason_code(reason: &str) -> bool {
    let bytes = reason.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.'))
}

/// Whether the step holds a live release of `credential` to one of this
/// install's runners: ending it is recorded even once the agent stopped
/// resolving (its project removed from the group, review M3).
async fn released_to_install<B: Backends>(
    backends: &B,
    runner: &RunnerPrincipal,
    work: &ActorWork<B::Execution>,
    sr: &StepRunId,
    credential: &str,
) -> Result<bool, Error> {
    let index = work.step_index(sr).map_err(|e| api(actor_error(e)))?;
    let runners: Vec<RunnerId> = work.runtime.state.steps[index]
        .runner
        .releases
        .iter()
        .filter(|r| r.credential_id == credential && r.revoked.is_none())
        .filter_map(|r| r.runner_id.parse().ok())
        .collect();
    for id in runners {
        if backends
            .runners()
            .runner_group(&runner.install_id, &id)
            .await
            .map_err(runner_error)?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The waiting reasons the runner's scheduler may report.
fn valid_reason(reason: &str) -> bool {
    matches!(
        reason,
        "agent_paused" | "agent_outside_ceiling" | "runner_offline"
    ) || reason.strip_prefix("limit:").is_some_and(|level| {
        !level.is_empty()
            && level.len() <= 64
            && level
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
    })
}

/// `POST /v1/runner-installs/{ri}/waiting` (directory token).
pub(crate) async fn waiting<B: Backends>(
    backends: &B,
    r: &Request,
    runner: &RunnerPrincipal,
) -> Result<(u16, Value), Error> {
    let request: RunnerWaitingRequest = crate::runners::decode(r)?;
    if request.items.len() > 100
        || request.items.iter().any(|i| {
            !valid_reason(&i.reason)
                || i.detail.as_ref().is_some_and(|d| {
                    d.trim().is_empty()
                        || d.chars().count() > 200
                        || d.chars().any(char::is_control)
                })
        })
    {
        return Err((422, "request.invalid"));
    }
    for item in &request.items {
        let (sr, scope, execution) =
            step_scope(backends, runner, item.step_run_id.as_str()).await?;
        let (_, work) = work(backends, &scope, &execution).await?;
        let agent =
            runbook_core::grants::target_agent(&work, &sr).map_err(|e| api(actor_error(e)))?;
        // Only steps aimed at this install's agents.
        if entry_of(backends, runner, agent).await?.is_none() {
            continue;
        }
        runbook_core::grants::waiting(work, &sr, &item.reason, item.detail.as_deref())
            .await
            .map_err(|e| api(actor_error(e)))?;
    }
    Ok((204, Value::Null))
}

/// `POST /v1/runner-installs/{ri}/events` (directory token): one org
/// webhook event, fanned out to every project's subscribers of its type.
pub(crate) async fn events<B: Backends>(
    backends: &B,
    r: &Request,
    runner: &RunnerPrincipal,
) -> Result<(u16, Value), Error> {
    use runbook_core::webhooks::{ProjectEvent, WebhookStore, derived_id, plan_project_fan_out};
    let request: RunnerEventRequest = crate::runners::decode(r)?;
    if request.kind == "runner.decision_cost" {
        let data: RunnerDecisionCost =
            serde_json::from_value(request.data).map_err(|_| (422, "request.invalid"))?;
        let (sr, scope, execution) =
            step_scope(backends, runner, data.step_run_id.as_str()).await?;
        let (_, work) = work(backends, &scope, &execution).await?;
        runbook_core::decisions::record_decision_cost(work, &sr, &data.lease_id, &data.cost)
            .await
            .map_err(|e| api(actor_error(e)))?;
        return Ok((204, Value::Null));
    }
    if request.kind == "runner.task_cost" {
        let data: RunnerTaskCost =
            serde_json::from_value(request.data).map_err(|_| (422, "request.invalid"))?;
        if backends
            .runners()
            .runner_group(&runner.install_id, &data.runner_id)
            .await
            .map_err(runner_error)?
            .is_none()
        {
            return Err((404, "claim.not_found"));
        }
        let (sr, scope, execution) =
            step_scope(backends, runner, data.step_run_id.as_str()).await?;
        let (_, work) = work(backends, &scope, &execution).await?;
        runbook_core::claims::record_task_cost(
            work,
            &sr,
            &data.claim_id,
            &data.runner_id,
            &data.cost,
        )
        .await
        .map_err(|e| api(actor_error(e)))?;
        return Ok((204, Value::Null));
    }
    if !RUNNER_EVENT_KINDS.contains(&request.kind.as_str())
        || !request.data.is_object()
        || request.data.to_string().len() > 16 * 1024
    {
        return Err((422, "request.invalid"));
    }
    let oauth = matches!(
        request.kind.as_str(),
        "credential.reconnect_required"
            | "credential.oauth_expiring"
            | "credential.oauth_refresh_failing"
    );
    let webhooks = backends.webhooks();
    let mut at_ms = webhooks
        .webhook_time_ms()
        .await
        .map_err(crate::runners::storage)?;
    let snapshot = backends
        .directory()
        .snapshot(&runner.org_id)
        .await
        .map_err(crate::runners::storage)?;
    let mut data = request.data.clone();
    let mut event_key = r.new_id.as_str();
    if oauth {
        let invalid = (422, "request.invalid");
        let event_id = request.data["event_id"]
            .as_str()
            .filter(|id| id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
            .ok_or(invalid)?;
        let credential = request.data["credential_id"]
            .as_str()
            .filter(|id| id.len() <= 128 && crate::runner_proxy::plain_segment(id))
            .ok_or(invalid)?;
        at_ms = request.data["occurred_at_ms"]
            .as_i64()
            .filter(|ms| *ms > 0 && *ms <= at_ms + 300_000)
            .ok_or(invalid)?;
        event_key = event_id;
        data = json!({"credential_id": credential});
        if request.kind == "credential.reconnect_required" {
            let reason = request.data["reason"]
                .as_str()
                .filter(|reason| {
                    matches!(
                        *reason,
                        "expiry_or_interrupted_refresh"
                            | "access_only_rejected"
                            | "invalid_binding"
                            | "client_authentication_unavailable"
                            | "invalid_grant"
                            | "invalid_client"
                            | "unauthorized_client"
                            | "refresh_failure_timeout"
                            | "ambiguous_refresh"
                            | "binding_changed"
                    )
                })
                .ok_or(invalid)?;
            data["reason"] = json!(reason);
        } else if request.kind == "credential.oauth_refresh_failing" {
            let since = request.data["since_ms"]
                .as_i64()
                .filter(|ms| *ms > 0 && *ms <= at_ms)
                .ok_or(invalid)?;
            data["since_ms"] = json!(since);
        } else {
            let expiry = request.data["expires_at_ms"]
                .as_i64()
                .filter(|ms| *ms > at_ms)
                .ok_or(invalid)?;
            data["expires_at_ms"] = json!(expiry);
        }
    }
    let budget = request.kind == "runner.budget_threshold";
    let budget_project = if budget {
        let invalid = (422, "request.invalid");
        event_key = request.data["event_id"]
            .as_str()
            .filter(|id| {
                id.len() == 36
                    && id.split('-').map(str::len).eq([8, 4, 4, 4, 12])
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
            })
            .ok_or(invalid)?;
        at_ms = request.data["occurred_at_ms"]
            .as_i64()
            .filter(|ms| *ms > 0 && *ms <= at_ms + 300_000)
            .ok_or(invalid)?;
        match budget_event_project(&request.data) {
            Some(project) => project,
            // Older scoped outbox rows did not identify a project. Drop
            // those rows rather than sending them to unrelated subscribers.
            None => return Ok((204, Value::Null)),
        }
    } else {
        None
    };
    data["install_id"] = json!(runner.install_id);
    for project in snapshot
        .projects
        .keys()
        .filter(|p| budget_project.is_none_or(|id| p.as_str() == id))
    {
        let scope = TenantScope {
            org_id: runner.org_id.clone(),
            project_id: project.clone(),
        };
        let event = ProjectEvent {
            event_id: if budget {
                derived_id(
                    "evt_",
                    0, // The outbox identity is independent of the reported occurrence time.
                    &[
                        &request.kind,
                        runner.install_id.as_str(),
                        event_key,
                        project.as_str(),
                    ],
                )
            } else {
                derived_id("evt_", at_ms, &[&request.kind, event_key, project.as_str()])
            },
            event_type: request.kind.clone(),
            at_ms,
            runbook_id: None,
            data: data.clone(),
        };
        let subscriptions = webhooks
            .list_subscriptions(&scope)
            .await
            .map_err(crate::runners::storage)?;
        let (record, deliveries) = plan_project_fan_out(&scope, &event, &subscriptions)
            .map_err(crate::runners::storage)?;
        if deliveries.is_empty() && !oauth && !budget {
            continue;
        }
        webhooks
            .record_event(&scope, record, deliveries, None)
            .await
            .map_err(crate::runners::storage)?;
    }
    Ok((204, Value::Null))
}

fn budget_event_project(data: &Value) -> Option<Option<&str>> {
    match data["level"].as_str()? {
        "project" => {
            let key = data["key"].as_str()?;
            (data["project_id"].is_null() || data["project_id"].as_str() == Some(key))
                .then_some(Some(key))
        }
        "agent" | "run" | "step" => Some(Some(data["project_id"].as_str()?)),
        "org" | "group" | "environment" => Some(None),
        _ => None,
    }
}

/// `GET /v1/runner-installs/{ri}/ready?group={g}&limit={n}` (directory token).
pub(crate) async fn ready<B: Backends>(
    backends: &B,
    r: &Request,
    runner: &RunnerPrincipal,
) -> Result<(u16, Value), Error> {
    let group: RunnerGroupId = r
        .query
        .get("group")
        .and_then(|g| g.parse().ok())
        .ok_or((422, "request.invalid"))?;
    let limit = match r.query.get("limit") {
        None => 50,
        Some(n) => n
            .parse::<u64>()
            .ok()
            .filter(|n| (1..=100).contains(n))
            .ok_or((422, "request.invalid"))?,
    };
    let invalid = || (422, "request.invalid");
    let project = r
        .query
        .get("project")
        .map(|project| project.parse::<ProjectId>())
        .transpose()
        .map_err(|_| invalid())?;
    let cursor = r
        .query
        .get("cursor")
        .map(|cursor| {
            if cursor.len() > 512 {
                return Err(invalid());
            }
            let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid())?;
            let (install, cursor_group, cursor_project, at, id): (
                RunnerInstallId,
                RunnerGroupId,
                Option<ProjectId>,
                i64,
                StepRunId,
            ) = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            if install != runner.install_id || cursor_group != group || cursor_project != project {
                return Err(invalid());
            }
            Ok((at, id))
        })
        .transpose()?;
    let mut items = backends
        .runners()
        .ready_steps(
            &runner.install_id,
            &group,
            project.as_ref(),
            cursor.as_ref().map(|(at, id)| (*at, id)),
            limit + 1,
            (r.query.get("decision_contract").is_some_and(|version| {
                version.parse::<u32>().ok() == Some(RUNNER_DECISION_CONTRACT)
            }))
            .then(|| backends.wall_clock_ms() - HEARTBEAT_STALE_MS),
        )
        .await
        .map_err(runner_error)?;
    let next_cursor = if items.len() > limit as usize {
        items.truncate(limit as usize);
        let last = &items[limit as usize - 1];
        let body = serde_json::to_vec(&(
            &runner.install_id,
            &group,
            &project,
            last.ready_at,
            &last.step_run_id,
        ))
        .map_err(crate::runners::storage)?;
        Some(URL_SAFE_NO_PAD.encode(body))
    } else {
        None
    };
    Ok((
        200,
        serde_json::to_value(ReadyStepPage { items, next_cursor })
            .map_err(crate::runners::storage)?,
    ))
}

/// The step ID locates the execution. The lease ID and runner principal
/// still have to match the execution's claim row before a write.
pub(crate) async fn finish_decision<B: Backends>(
    backends: &B,
    r: &Request,
    runner: &RunnerPrincipal,
    lease: &str,
    decide: bool,
) -> Result<(u16, Value), Error> {
    use runbook_core::decisions::{DecisionOutcome, finish_decision};
    let runner_id = runner.runner_id.as_ref().ok_or((403, "auth.forbidden"))?;
    let lease = lease.parse().map_err(|_| (404, "claim.not_found"))?;
    let step = r.query.get("step_run_id").ok_or((422, "request.invalid"))?;
    let (sr, scope, execution) = step_scope(backends, runner, step).await?;
    let (store, work) = work(backends, &scope, &execution).await?;
    let post = post_identity(r, &runner_key(runner_id)).map_err(api)?;
    let replay = Replay {
        key: post.key,
        principal: post.principal,
        request_hash: post.request_hash,
    };
    let outcome = if decide {
        DecisionOutcome::Decide(crate::runners::decode(r)?)
    } else {
        DecisionOutcome::Abstain(crate::runners::decode(r)?)
    };
    finish_decision(&store, work, runner_id, &sr, &lease, outcome, &replay)
        .await
        .map(|value| (200, value))
        .map_err(|e| api(actor_error(e)))
}

#[cfg(test)]
mod budget_event_tests {
    use super::*;

    #[test]
    fn scoped_budget_events_require_the_owning_project() {
        assert_eq!(
            budget_event_project(&json!({"level":"project","key":"project-a"})),
            Some(Some("project-a"))
        );
        assert_eq!(
            budget_event_project(
                &json!({"level":"project","key":"project-a","project_id":"project-b"})
            ),
            None
        );
        assert_eq!(
            budget_event_project(&json!({"level":"agent","key":"agent-a"})),
            None
        );
        assert_eq!(
            budget_event_project(
                &json!({"level":"agent","key":"agent-a","project_id":"project-b"})
            ),
            Some(Some("project-b"))
        );
        assert_eq!(budget_event_project(&json!({"level":"org"})), Some(None));
    }
}
