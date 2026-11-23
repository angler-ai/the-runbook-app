//! D32 execution credentials disclose only the owning run's step-at-a-time views.
use crate::{
    Request,
    actor_requests::{ActorRequest, decode, post_identity, validate_declaration},
    actor_store::{Identity, actor_error},
    actors::ActorTarget,
    backends::Backends,
    execution_credentials::{access, storage},
    executions::ExecutionApiError as Error,
    problem::Response,
};
use runbook_core::{
    actor::{ActorWork, Replay, begin_actor},
    catalog::CatalogStore,
    credentials::{TokenKind, parse_secret},
    step_lookup::StepRunLookup,
};
use runbook_model::*;
use runbook_spec::ir::NodeKind;
use serde_json::{Value, json};

pub fn is_execution(r: &Request) -> bool {
    r.bearer()
        .is_some_and(|s| parse_secret(s).is_ok_and(|(k, _)| k == TokenKind::Execution))
}
fn forbidden() -> Error {
    Error::new(403, "auth.forbidden")
}
pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok(value) => Response::json(200, value, &r.request_id),
        Err(e) => {
            let mut out = Response::error(e.status, e.code, &r.request_id);
            out.body["errors"] = json!(e.diagnostics);
            out
        }
    }
}
async fn dispatch<B: Backends>(backends: &B, r: &Request) -> Result<Value, Error> {
    let secret = r.bearer().ok_or_else(forbidden)?;
    let verified = access(backends, secret).await?;
    // Exact route allow-list. Never reuse issuer permissions for this credential.
    let mut op = "read";
    let mut params = crate::directory::match_path("/v1/executions/{exe}", &r.path);
    if params.is_none() {
        params = crate::directory::match_path("/v1/executions/{exe}/next", &r.path);
        op = "next";
    }
    if params.is_none() {
        params = crate::directory::match_path("/v1/step-runs/{sr}", &r.path);
        op = "step";
    }
    if params.is_none() {
        params = crate::directory::match_path("/v1/step-runs/{sr}/skill-files", &r.path);
        op = "skill";
    }
    if params.is_none() {
        for (method, path, operation) in crate::actors::served_routes() {
            if *method == "POST"
                && !matches!(*operation, "signal" | "patch" | "override")
                && let Some(p) = crate::directory::match_path(path, &r.path)
            {
                params = Some(p);
                op = operation;
                break;
            }
        }
    }
    let params = params.ok_or_else(forbidden)?;
    let read = matches!(op, "read" | "next" | "step" | "skill");
    if r.method != if read { "GET" } else { "POST" } {
        return Err(forbidden());
    }
    let store = backends.execution(verified.scope.clone());
    if params
        .get("exe")
        .is_some_and(|e| e != verified.principal.execution_id.as_str())
    {
        return Err(forbidden());
    }
    let step = if let Some(sr) = params.get("sr") {
        let sr: StepRunId = sr.parse().map_err(|_| forbidden())?;
        let found = store
            .step_run_execution(&verified.scope.org_id, &sr)
            .await
            .map_err(storage)?;
        if found
            != Some((
                verified.principal.execution_id.clone(),
                verified.scope.project_id.clone(),
            ))
        {
            return Err(forbidden());
        }
        Some(sr)
    } else {
        None
    };
    if op == "release" && r.body.get("claim").is_none() {
        return Err(forbidden());
    }
    let work = begin_actor(
        &store,
        &backends.directory(),
        verified.scope.clone(),
        &verified.principal.execution_id,
    )
    .await
    .map_err(actor_error)?;
    // Recheck revocation/issuer after waiting for the execution lock.
    let current = access(backends, secret).await?;
    let identity = Identity {
        service_attribution: crate::actor_store::service_attribution(
            backends,
            &verified.scope.org_id,
            &current.principal.issuer,
        )
        .await?,
        key: format!("execution:{}", current.principal.family_id),
        human: None,
        credential_ref: current.record.token_id.to_string(),
    };
    if read {
        if matches!(op, "read" | "next") {
            return serde_json::to_value(brief(&work)?).map_err(storage);
        }
        let sr = step.as_ref().ok_or_else(forbidden)?;
        let claim = work.snapshot.claims.get(sr).ok_or_else(forbidden)?;
        let index = work.step_index(sr).map_err(actor_error)?;
        if claim.released
            || claim.session.principal != identity.key
            || claim
                .lease_deadline_at_ms
                .is_some_and(|t| t <= work.at_ms())
            || work.runtime.state.steps[index].status != StepStatus::Claimed
        {
            return Err(forbidden());
        }
        return claimed_read::<B>(&store, work, sr, op, r).await;
    }
    let replay = post_identity(r, &identity.key)?;
    let target = ActorTarget {
        operation: op,
        scope: verified.scope,
        execution_id: verified.principal.execution_id,
        step_run_id: step,
        signal_name: None,
    };
    let operation = crate::actor_store::operation(&target, &replay)?;
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
        .map_err(actor_error)?
    {
        return Ok(saved);
    }
    let request = decode(
        op,
        target.step_run_id.as_ref().map_or("", StepRunId::as_str),
        r.body.clone(),
        &[],
    )?;
    match &request {
        ActorRequest::Claim(c) => validate_declaration(
            ActorKind::Agent,
            Some(&c.session_id),
            c.actor.as_ref(),
            true,
        )?,
        ActorRequest::Decision(d) => validate_declaration(
            ActorKind::Agent,
            d.session_id.as_deref(),
            d.actor.as_ref(),
            false,
        )?,
        _ => (),
    }
    crate::actor_store::execute_ready::<B>(
        &store,
        work,
        identity,
        target,
        request,
        r.new_id.clone(),
        Some(replay),
    )
    .await
}
/// D32: an execution token reads the shared resume brief for its agent audience.
fn brief<S: runbook_core::session::SessionStore>(
    work: &ActorWork<S>,
) -> Result<ResumeBrief, Error> {
    Ok(runbook_core::reads::resume_brief(
        &work.runtime.state,
        Some(ActorKind::Agent),
    ))
}
pub(crate) async fn claimed_read<B: Backends>(
    store: &B::Execution,
    work: ActorWork<B::Execution>,
    sr: &StepRunId,
    op: &str,
    r: &Request,
) -> Result<Value, Error> {
    let state = &work.runtime.state;
    let step = &state.steps[work.step_index(sr).map_err(actor_error)?];
    let node = state.node(&step.step_id).ok_or_else(forbidden)?;
    if let NodeKind::Approval(approval) = &node.kind
        && op == "step"
        && runbook_engine::decision_open(state, step)
    {
        return Ok(json!(ClaimedDecisionRead::Decision {
            step_run_id: sr.clone(),
            decision: runbook_core::decisions::decision_input(state, step, approval),
        }));
    }
    let NodeKind::Task(task) = &node.kind else {
        return Err(forbidden());
    };
    let skills_hashes = crate::step_reads::pinned_skills(state, task);
    if op == "skill" {
        let hash = if let Some(target_skill) = r.query.get("skill") {
            skills_hashes
                .iter()
                .find(|(name, h)| {
                    name == target_skill || &runbook_core::authoring::hex(h) == target_skill
                })
                .map(|(_, h)| h)
                .ok_or_else(forbidden)?
        } else {
            skills_hashes
                .first()
                .map(|(_, h)| h)
                .ok_or_else(forbidden)?
        };
        let path = r.query.get("path").map_or("SKILL.md", String::as_str);
        runbook_core::skills::validate_path(path).map_err(|_| forbidden())?;
        let bytes = runbook_core::skill_library::read_pinned_file(store, store, hash, path)
            .await
            .map_err(|_| Error::new(404, "resource.not_found"))?;
        use base64::Engine;
        return Ok(
            json!({"path":path,"content":base64::engine::general_purpose::STANDARD.encode(bytes)}),
        );
    }
    let rendered = runbook_engine::render_step(state, sr, ActorKind::Agent)
        .map_err(|e| actor_error(e.into()))?;
    let template = runbook_engine::step_template(state, sr, ActorKind::Agent)
        .map_err(|e| actor_error(e.into()))?;
    let mut skills_payload = Vec::new();
    for (_, hash) in &skills_hashes {
        let version = store
            .skill_version(hash)
            .await
            .map_err(storage)?
            .ok_or_else(|| Error::new(404, "resource.not_found"))?;
        let bytes = runbook_core::skill_library::read_pinned_file(store, store, hash, "SKILL.md")
            .await
            .map_err(storage)?;
        skills_payload.push(
            json!({"hash":runbook_core::authoring::hex(hash),"body":String::from_utf8(bytes).map_err(storage)?,"files":version.manifest.entries}),
        );
    }
    let skill = skills_payload.first().cloned();
    let mut body = crate::step_reads::step_body(
        sr,
        rendered,
        template,
        skill,
        skills_payload,
        &task.evidence,
    );
    if matches!(
        task.agent_target,
        Some(runbook_spec::ir::AgentTarget::Runner { .. })
    ) {
        body["runner_limits"] = json!(runbook_model::RunnerStepLimits {
            model: task
                .agent_limits
                .as_ref()
                .and_then(|limits| limits.model.clone()),
            max_tokens: task
                .agent_limits
                .as_ref()
                .and_then(|limits| limits.max_tokens),
            timeout_ms: node.timeout.map(|duration| duration.millis),
            check: task.check,
        });
    }
    // D41: the step's memory as labeled data; the read is recorded when its
    // refs differ from the last one recorded for this step run.
    let (block, refs) = crate::memory::step_memory(
        store,
        &work.runtime.scope,
        &work.record.runbook_id,
        &step.step_id,
    )
    .await?;
    body["memory"] = block;
    let owner = work
        .snapshot
        .claims
        .get(sr)
        .map(|claim| claim.owner.clone())
        .ok_or_else(forbidden)?;
    runbook_core::memory_entries::record_step_read(work, sr, refs, owner)
        .await
        .map_err(actor_error)?;
    Ok(body)
}
