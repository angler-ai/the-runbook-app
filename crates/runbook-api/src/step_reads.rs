//! B8a: step-at-a-time reads for human PATs and service tokens. Execution
//! tokens never reach this module (`execution_access` owns their D32
//! allow-list). A reader needs `executions:read` on the run's project; the
//! run or step run is found by an org-scoped key read, and a run outside the
//! caller's reach answers 404 so its existence is not disclosed.
//!
//! F46: `GET /v1/step-runs/{sr}/logs?cursor=<seq>&limit=<n>` pages the
//! step's persisted (redacted) lines by sequence (D33). A purged run's log
//! link answers 410 `execution.history_expired` to a caller who may read
//! its project.
use crate::{
    Request, backends::Backends, execution_credentials::storage,
    executions::ExecutionApiError as Error, problem::Response,
};
use runbook_core::{
    ExecutionStore,
    catalog::{CatalogStore, TenantScope},
    credentials::{
        AuthError, CredentialStore, Principal, Scope, TokenKind, authorize, parse_secret,
    },
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    start::StartStore,
    step_lookup::StepRunLookup,
};
use runbook_engine::ExecutionState;
use runbook_model::*;
use runbook_spec::ir::NodeKind;
use serde_json::{Value, json};

const ROUTES: [(&str, &str); 3] = [
    ("/v1/executions/{exe}/next", "next"),
    ("/v1/step-runs/{sr}", "step"),
    ("/v1/step-runs/{sr}/skill-files", "skill"),
];

/// The log read; POSTs to the same path append through the actor routes.
pub fn recognizes_logs(method: &str, path: &str) -> bool {
    method == "GET" && crate::directory::match_path(LOGS, path).is_some()
}
const LOGS: &str = "/v1/step-runs/{sr}/logs";
const LOG_PAGE: usize = 100;
const LOG_PAGE_MAX: usize = 1000;

/// `cursor` is the last sequence read (default 0); `limit` 1..=1000.
fn log_query(r: &Request) -> Result<(u64, usize), Error> {
    let bad = || Error::new(400, "request.invalid_query");
    let (mut cursor, mut limit) = (0, LOG_PAGE);
    for (key, value) in &r.query {
        let digits = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
        match key.as_str() {
            "cursor" if digits => cursor = value.parse().map_err(|_| bad())?,
            "limit" if digits => {
                limit = value.parse().map_err(|_| bad())?;
                if !(1..=LOG_PAGE_MAX).contains(&limit) {
                    return Err(bad());
                }
            }
            _ => return Err(bad()),
        }
    }
    Ok((cursor, limit))
}

pub fn recognizes(path: &str) -> bool {
    ROUTES
        .iter()
        .any(|(pattern, _)| crate::directory::match_path(pattern, path).is_some())
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok(value) => Response::json(200, value, &r.request_id),
        Err(e) => Response::error(e.status, e.code, &r.request_id),
    }
}

fn not_found() -> Error {
    Error::new(404, "resource.not_found")
}

async fn dispatch<B: Backends>(backends: &B, r: &Request) -> Result<Value, Error> {
    let (op, params) = ROUTES
        .iter()
        .chain(&[(LOGS, "logs")])
        .find_map(|(pattern, op)| crate::directory::match_path(pattern, &r.path).map(|p| (*op, p)))
        .ok_or_else(not_found)?;
    if r.method != "GET" {
        return Err(Error::new(405, "request.method_not_allowed"));
    }
    let invalid = || Error::new(401, "auth.invalid_credential");
    if r.bearer()
        .is_some_and(|s| parse_secret(s).is_ok_and(|(k, _)| k == TokenKind::Execution))
    {
        return Err(Error::new(403, "auth.forbidden"));
    }
    let credentials = backends.credentials();
    let record = r.credential_record(&credentials).await.map_err(|e| {
        let (status, code) = e.status();
        Error::new(status, code)
    })?;
    let directory = backends.directory();
    let now = directory.current_time_ms().await.map_err(storage)?;
    if record.validate().is_err()
        || record.revoked_at.is_some()
        || record.expires_at.is_some_and(|t| t <= now)
    {
        return Err(invalid());
    }
    // The scope check needs no resource, so it discloses nothing.
    if !record.scopes.contains(&Scope::ExecutionsRead) {
        return Err(Error::new(403, "auth.scope_missing"));
    }
    let snapshot = directory.snapshot(&record.org_id).await.map_err(storage)?;
    // Org-scoped key reads; the project half of this scope is unused by them.
    let lookup = backends.execution(TenantScope {
        org_id: record.org_id.clone(),
        project_id: snapshot
            .projects
            .keys()
            .next()
            .cloned()
            .ok_or_else(not_found)?,
    });
    let page = if op == "logs" {
        Some(log_query(r)?)
    } else {
        None
    };
    let mut expired = false;
    let (execution, project, step) = if let Some(sr) = params.get("sr") {
        let sr: StepRunId = sr.parse().map_err(|_| not_found())?;
        let mut found = lookup
            .step_run_execution(&record.org_id, &sr)
            .await
            .map_err(storage)?;
        if found.is_none() && page.is_some() {
            found = backends
                .expired_step_run(&record.org_id, &sr)
                .await
                .map_err(storage)?;
            expired = found.is_some();
        }
        let (exe, project) = found.ok_or_else(not_found)?;
        (exe, project, Some(sr))
    } else {
        let exe: ExecutionId = params["exe"].parse().map_err(|_| not_found())?;
        let project = lookup
            .execution_project(&record.org_id, &exe)
            .await
            .map_err(storage)?
            .ok_or_else(not_found)?;
        (exe, project, None)
    };
    let principal = authorize(&record, &snapshot, now, &project, Scope::ExecutionsRead).map_err(
        |e| match e {
            AuthError::Forbidden => not_found(),
            _ => invalid(),
        },
    )?;
    credentials
        .touch_last_used(&record.org_id, &record.id, now)
        .await
        .map_err(storage)?;
    let audience = match principal {
        Principal::Human { .. } => ActorKind::Human,
        Principal::Agent { .. } | Principal::Service { .. } => ActorKind::Agent,
    };
    let scope = TenantScope {
        org_id: record.org_id.clone(),
        project_id: project,
    };
    let store_scope = scope.clone();
    let store = backends.execution(scope);
    // Authorization above precedes any disclosure that the run expired.
    if expired {
        return Err(Error::new(410, "execution.history_expired"));
    }
    if let (Some((cursor, limit)), Some(sr)) = (page, &step) {
        return log_page::<B>(&store, &execution, sr, cursor, limit).await;
    }
    let run = store.load(&execution).await.map_err(|e| match e {
        runbook_core::StoreError::NotFound => not_found(),
        _ => Error::new(503, "storage.unavailable"),
    })?;
    let record = store
        .start_record(&execution)
        .await
        .map_err(storage)?
        .ok_or_else(not_found)?;
    let state = crate::executions::restore_state(&store, &record, &run).await?;
    let Some(sr) = step else {
        return serde_json::to_value(runbook_core::reads::resume_brief(&state, Some(audience)))
            .map_err(storage);
    };
    let mut body = step_read::<B>(&store, &state, &sr, op, audience, r).await?;
    if op != "step" {
        return Ok(body);
    }
    // D41: the step's memory as labeled data. The read is recorded only for
    // the caller who holds the step's claim (the one doing the work).
    let path = state
        .steps
        .iter()
        .find(|s| s.id.as_ref() == Some(&sr))
        .map(|s| s.step_id.clone())
        .ok_or_else(not_found)?;
    let scope = store_scope;
    let (block, refs) =
        crate::memory::step_memory(&store, &scope, &record.runbook_id, &path).await?;
    body["memory"] = block;
    use runbook_core::claims::ClaimStore;
    let key = crate::actor_store::principal_key(&principal);
    if let Some(claim) = store.claim_record(&execution, &sr).await.map_err(storage)?
        && !claim.released
        && claim.session.principal == key
    {
        let work =
            runbook_core::actor::begin_actor(&store, &backends.directory(), scope, &execution)
                .await
                .map_err(crate::actor_store::actor_error)?;
        let claimed = work
            .runtime
            .state
            .steps
            .iter()
            .any(|s| s.id.as_ref() == Some(&sr) && s.status == StepStatus::Claimed);
        if claimed {
            runbook_core::memory_entries::record_step_read(work, &sr, refs, claim.owner)
                .await
                .map_err(crate::actor_store::actor_error)?;
        }
    }
    Ok(body)
}

async fn log_page<B: Backends>(
    store: &B::Execution,
    execution: &ExecutionId,
    sr: &StepRunId,
    cursor: u64,
    limit: usize,
) -> Result<Value, Error> {
    use runbook_core::claims::ClaimStore;
    let mut rows = store
        .read_logs(execution, sr, cursor, limit + 1)
        .await
        .map_err(|e| match e {
            runbook_core::StoreError::NotFound => Error::new(410, "execution.history_expired"),
            _ => Error::new(503, "storage.unavailable"),
        })?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next = rows.last().map_or(cursor, |(seq, _)| *seq);
    serde_json::to_value(StepLogPage {
        lines: rows
            .into_iter()
            .map(|(seq, line)| StepLogLine { seq, line })
            .collect(),
        next_cursor: next.to_string(),
        has_more,
    })
    .map_err(storage)
}

/// D32 step-at-a-time disclosure for project readers: instructions of a step
/// are disclosed once it has been offered to an actor (ready, claimed,
/// waiting after a report, or finished). Steps skipped by their condition
/// were never offered and stay withheld; pending steps have no step run ID.
fn disclosed(step: &runbook_engine::StepState) -> bool {
    match step.status {
        StepStatus::Pending => false,
        StepStatus::Skipped => step.skip_reason.is_none(),
        _ => true,
    }
}

/// A task's pinned skill hash. Pins are keyed by the task's reference as
/// written: a `skill://` URI or a `spec.skills` alias, not the alias target.
pub fn pinned_skill<'a>(
    state: &'a ExecutionState,
    task: &runbook_spec::ir::Task,
) -> Option<&'a [u8; 32]> {
    let name = task.skill.as_ref()?;
    let hashes = state.compiled().skill_hashes();
    hashes.get(name).or_else(|| {
        let target = state.compiled().ir().skills.get(name)?;
        hashes.get(target)
    })
}

/// LANG8: All pinned skill hashes for a task.
pub fn pinned_skills(
    state: &ExecutionState,
    task: &runbook_spec::ir::Task,
) -> Vec<(String, [u8; 32])> {
    let hashes = state.compiled().skill_hashes();
    task.skills
        .iter()
        .filter_map(|name| {
            let h = hashes.get(name).or_else(|| {
                let target = state.compiled().ir().skills.get(name)?;
                hashes.get(target)
            })?;
            Some((name.clone(), *h))
        })
        .collect()
}

async fn step_read<B: Backends>(
    store: &B::Execution,
    state: &ExecutionState,
    sr: &StepRunId,
    op: &str,
    audience: ActorKind,
    r: &Request,
) -> Result<Value, Error> {
    let step = state
        .steps
        .iter()
        .find(|s| s.id.as_ref() == Some(sr))
        .ok_or_else(not_found)?;
    let node = state.node(&step.step_id).ok_or_else(not_found)?;
    let NodeKind::Task(task) = &node.kind else {
        return Err(Error::new(409, "step.not_task"));
    };
    if !disclosed(step) {
        return Err(Error::new(409, "step.not_ready"));
    }
    let skills_hashes = pinned_skills(state, task);
    if op == "skill" {
        let hash = if let Some(target_skill) = r.query.get("skill") {
            skills_hashes
                .iter()
                .find(|(name, h)| {
                    name == target_skill || &runbook_core::authoring::hex(h) == target_skill
                })
                .map(|(_, h)| h)
                .ok_or_else(not_found)?
        } else {
            skills_hashes
                .first()
                .map(|(_, h)| h)
                .ok_or_else(not_found)?
        };
        let path = r.query.get("path").map_or("SKILL.md", String::as_str);
        runbook_core::skills::validate_path(path).map_err(|_| not_found())?;
        let bytes = runbook_core::skill_library::read_pinned_file(store, store, hash, path)
            .await
            .map_err(|_| not_found())?;
        use base64::Engine;
        return Ok(
            json!({"path":path,"content":base64::engine::general_purpose::STANDARD.encode(bytes)}),
        );
    }
    let rendered = runbook_engine::render_step(state, sr, audience)
        .map_err(|_| Error::new(409, "step.not_renderable"))?;
    let template = runbook_engine::step_template(state, sr, audience)
        .map_err(|_| Error::new(409, "step.not_renderable"))?;
    let mut skills_payload = Vec::new();
    for (_, hash) in &skills_hashes {
        let version = store
            .skill_version(hash)
            .await
            .map_err(storage)?
            .ok_or_else(not_found)?;
        let bytes = runbook_core::skill_library::read_pinned_file(store, store, hash, "SKILL.md")
            .await
            .map_err(storage)?;
        skills_payload.push(
            json!({"hash":runbook_core::authoring::hex(hash),"body":String::from_utf8(bytes).map_err(storage)?,"files":version.manifest.entries}),
        );
    }
    let skill = skills_payload.first().cloned();
    Ok(step_body(
        sr,
        rendered,
        template,
        skill,
        skills_payload,
        &task.evidence,
    ))
}

/// The `ClaimedStepRead` body shared with execution-token reads. F62: the
/// rendered text stays for compatibility; `template` carries the author's
/// text and the bound run data apart. CF12c: `evidence` is the task's
/// evidence policy.
pub fn step_body(
    sr: &StepRunId,
    rendered: runbook_engine::RenderedStep,
    template: StepTemplate,
    skill: Option<Value>,
    skills: Vec<Value>,
    evidence: &runbook_spec::ir::Evidence,
) -> Value {
    let kinds = |kinds: &[runbook_spec::ir::EvidenceKind]| {
        kinds.iter().map(|k| k.as_str().to_owned()).collect()
    };
    let evidence = runbook_model::StepEvidencePolicy {
        required: kinds(&evidence.required),
        accepted: evidence.accepted.as_deref().map(kinds),
    };
    json!({
        "step_run_id": sr,
        "instructions": rendered.instructions,
        "checklist": rendered.checklist,
        "parameters": rendered.skill_parameters,
        "output_schema": rendered.output_schema,
        "skill": skill,
        "skills": skills,
        "template": template,
        "evidence": evidence
    })
}
