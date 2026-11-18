//! B6b actor backend over any [`Backends`] (CF0: moved from the native
//! server). Futures intentionally retain the core's non-Send boundary.
use crate::{
    actor_requests::{ActorReplay, ActorRequest},
    actors::{ActorBackend, ActorInvocation, ActorTarget, ApiError},
    backends::Backends,
};
use runbook_core::{
    StoreError,
    actor::{ActorError, ActorPrincipal, ActorWork, Replay, begin_actor},
    amendments,
    catalog::{CatalogStore, TenantScope},
    claims, control,
    credentials::{AuthError, CredentialStore, Principal, Scope, authorize},
    decisions,
    session::{ExecutionSession, SessionStore},
    step_lookup::StepRunLookup,
};
use runbook_model::*;
use serde_json::Value;

#[derive(Clone)]
pub struct StoreActorBackend<B> {
    pub backends: B,
}

fn store_error(error: StoreError) -> ApiError {
    match error {
        StoreError::NotFound => ApiError::new(404, "resource.not_found"),
        StoreError::Backend(_) => ApiError::new(503, "storage.unavailable"),
        StoreError::InvalidBatch(code) => rejection(code, vec![]),
    }
}
fn rejection(code: &'static str, fields: Vec<FieldError>) -> ApiError {
    let code = if code == "queue.stale_lease" {
        "claim.stale_generation"
    } else {
        code
    };
    let status = match code {
        "auth.required" | "auth.invalid_credential" => 401,
        "auth.forbidden"
        | "auth.scope_missing"
        | "approval.not_eligible"
        | "claim.not_assignee"
        | "amendment.admin_required"
        | "amendment.human_required" => 403,
        "resource.not_found" | "execution.not_found" | "step.not_found" | "claim.not_found" => 404,
        "request.idempotency_key_reused"
        | "claim.stale_generation"
        | "claim.conflict"
        | "approval.conflict"
        | "execution.conflict"
        | "signal.not_waiting"
        | "execution.paused"
        | "execution.closed"
        | "execution.not_started"
        | "step.not_claimable"
        | "claim.take_over_live_claim"
        | "claim.generation_exhausted"
        | "approval.not_open"
        | "approval.deadline_exceeded"
        | "approval.duplicate_vote"
        | "approval.automatic_pending"
        | "execution.not_running"
        | "execution.cleanup_only"
        | "execution.deadline_exceeded"
        | "execution.cleanup_deadline_exceeded"
        | "step.deadline_exceeded"
        | "signal.duplicate"
        | "signal.id_conflict"
        | "amendment.conflict"
        | "amendment.step_not_finished" => 409,
        _ => 422,
    };
    ApiError {
        status,
        code,
        diagnostics: fields
            .into_iter()
            .map(|f| Diagnostic {
                path: f.path,
                code: f.code,
                message: f.message,
                line: None,
                column: None,
            })
            .collect(),
    }
}
pub fn actor_error(error: ActorError) -> ApiError {
    match error {
        ActorError::Store(error) => store_error(error),
        ActorError::Rejected { code, errors } => rejection(code, errors),
        ActorError::Engine(runbook_engine::EngineError::Invariant { .. }) => {
            ApiError::new(503, "storage.unavailable")
        }
        ActorError::Engine(error) => rejection(error.code(), error.errors().to_vec()),
    }
}
fn replay(identity: &ActorReplay) -> Replay {
    Replay {
        key: identity.key.clone(),
        principal: identity.principal.clone(),
        request_hash: identity.request_hash,
    }
}
pub fn principal_key(principal: &Principal) -> String {
    match principal {
        Principal::Human { user_id } => format!("human:{user_id}"),
        Principal::Agent { sponsor } => format!("agent:{sponsor}"),
        Principal::Service { token_id, .. } => format!("service:{token_id}"),
    }
}
pub fn operation(target: &ActorTarget, identity: &ActorReplay) -> Result<String, ApiError> {
    let namespace = match target.operation {
        "claim" => "claim_step",
        "heartbeat" => "heartbeat",
        "logs" => "append_logs",
        "report" => "report_step",
        "checklist" => "update_checklist",
        "release" => "release_claim",
        "admin_release" => "admin_release",
        "override" => "override",
        "decision" if identity.principal.starts_with("human:") => "vote",
        "decision" => "agent_decide",
        "signal" => return Ok("signal".into()),
        "patch" => return Ok("patch".into()),
        _ => return Err(ApiError::new(422, "request.invalid")),
    };
    Ok(format!("{namespace}:{}", step(target)?))
}
fn step(target: &ActorTarget) -> Result<&StepRunId, ApiError> {
    target
        .step_run_id
        .as_ref()
        .ok_or_else(|| ApiError::new(422, "request.invalid"))
}
fn json(value: impl serde::Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|_| ApiError::new(503, "storage.unavailable"))
}

impl<B: Backends> StoreActorBackend<B> {
    fn scoped(&self, scope: TenantScope) -> B::Execution {
        self.backends.execution(scope)
    }
    fn org_store(&self, org: &OrganizationId) -> B::Execution {
        // Both callers use explicitly org-keyed queries; the sentinel is never queried.
        crate::backends::org_store(&self.backends, org)
    }
}

/// Credential-derived identity shared by PAT/service and execution families.
pub struct Identity {
    pub key: String,
    pub human: Option<UserId>,
    pub credential_ref: String,
}
impl Identity {
    fn from_principal(p: &Principal, credential_ref: String) -> Self {
        Self {
            key: principal_key(p),
            human: match p {
                Principal::Human { user_id } => Some(user_id.clone()),
                _ => None,
            },
            credential_ref,
        }
    }
}
fn declared_principal(
    identity: &Identity,
    session_id: Option<&str>,
    declaration: Option<&ActorDeclaration>,
) -> Result<ActorPrincipal, ApiError> {
    let actor = if let Some(user_id) = &identity.human {
        ActorRef::Human {
            user_id: user_id.clone(),
        }
    } else {
        let session = session_id
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ApiError::new(422, "request.invalid_actor"))?;
        let declaration = declaration
            .filter(|d| d.kind == ActorKind::Agent)
            .ok_or_else(|| ApiError::new(422, "request.invalid_actor"))?;
        let name = declaration
            .agent_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ApiError::new(422, "request.invalid_actor"))?;
        ActorRef::Agent {
            principal_id: identity.key.clone(),
            session_id: session.into(),
            name: name.into(),
            model: declaration.model.clone(),
        }
    };
    Ok(ActorPrincipal {
        principal: identity.key.clone(),
        actor,
        credential_ref: identity.credential_ref.clone(),
    })
}
fn claim_principal<S: SessionStore>(
    work: &ActorWork<S>,
    identity: &Identity,
    step: &StepRunId,
) -> Result<ActorPrincipal, ApiError> {
    let current = work
        .snapshot
        .claims
        .get(step)
        .ok_or_else(|| ApiError::new(404, "claim.not_found"))?;
    if current.session.principal != identity.key {
        return Err(ApiError::new(403, "auth.forbidden"));
    }
    let actor = if let Some(user_id) = &identity.human {
        ActorRef::Human {
            user_id: user_id.clone(),
        }
    } else {
        if !matches!(&current.owner,ActorRef::Agent{principal_id,..} if principal_id==&identity.key)
        {
            return Err(ApiError::new(403, "auth.forbidden"));
        }
        current.owner.clone()
    };
    Ok(ActorPrincipal {
        principal: identity.key.clone(),
        actor,
        credential_ref: identity.credential_ref.clone(),
    })
}

impl<B: Backends> ActorBackend for StoreActorBackend<B> {
    async fn step_run_execution(
        &self,
        org: &OrganizationId,
        step: &StepRunId,
    ) -> Result<Option<(ExecutionId, ProjectId)>, ApiError> {
        StepRunLookup::step_run_execution(&self.org_store(org), org, step)
            .await
            .map_err(store_error)
    }
    async fn execution_project(
        &self,
        org: &OrganizationId,
        execution: &ExecutionId,
    ) -> Result<Option<ProjectId>, ApiError> {
        self.org_store(org)
            .execution_project(org, execution)
            .await
            .map_err(store_error)
    }
    async fn find_replay(
        &self,
        target: &ActorTarget,
        identity: &ActorReplay,
    ) -> Result<Option<Value>, ApiError> {
        let store = self.scoped(target.scope.clone());
        let session = store
            .begin(&target.execution_id)
            .await
            .map_err(store_error)?;
        let replay = replay(identity);
        let operation = operation(target, identity)?;
        let row = session
            .actor_replay(&replay.prefix(&operation).map_err(store_error)?)
            .await
            .map_err(store_error)?;
        replay
            .lookup::<Value>(&operation, row.iter(), session.transaction_time_ms())
            .map_err(store_error)
    }
    async fn execute(&self, invocation: ActorInvocation) -> Result<Value, ApiError> {
        let ActorInvocation {
            target,
            principal,
            credential,
            replay: identity,
            request,
            new_id,
        } = invocation;
        let store = self.scoped(target.scope.clone());
        let directory = self.backends.directory();
        let work = begin_actor(
            &store,
            &directory,
            target.scope.clone(),
            &target.execution_id,
        )
        .await
        .map_err(actor_error)?;
        // Recheck the credential where the run is: a stored token is read
        // again (it may have been revoked meanwhile). A session's record is
        // never stored; it was resolved from the auth service for this very
        // request, so it is used as is, and `authorize` below still checks
        // its expiry and the current memberships.
        let current = if runbook_core::auth_links::is_session_record(&credential) {
            if credential.org_id != target.scope.org_id {
                return Err(ApiError::new(401, "auth.invalid_credential"));
            }
            credential.clone()
        } else {
            let current = self
                .backends
                .credentials()
                .find_by_hash(&credential.hash)
                .await
                .map_err(|_| ApiError::new(503, "storage.unavailable"))?
                .ok_or_else(|| ApiError::new(401, "auth.invalid_credential"))?;
            if current.id != credential.id
                || current.org_id != target.scope.org_id
                || current.kind != credential.kind
            {
                return Err(ApiError::new(401, "auth.invalid_credential"));
            }
            current
        };
        let permission = if matches!(target.operation, "patch" | "override") {
            Scope::ExecutionsAdmin
        } else {
            Scope::ExecutionsAct
        };
        let verified = authorize(
            &current,
            &work.runtime.directory,
            work.at_ms(),
            &target.scope.project_id,
            permission,
        )
        .map_err(|error| match error {
            AuthError::Forbidden => ApiError::new(
                403,
                if current.scopes.contains(&permission) {
                    "auth.forbidden"
                } else {
                    "auth.scope_missing"
                },
            ),
            _ => ApiError::new(401, "auth.invalid_credential"),
        })?;
        if verified != principal {
            return Err(ApiError::new(403, "auth.forbidden"));
        }
        let replay = identity.as_ref().map(replay);
        if replay
            .as_ref()
            .is_some_and(|r| r.principal != principal_key(&verified))
        {
            return Err(ApiError::new(403, "auth.forbidden"));
        }
        // Recheck the indexed durable response before reading mutable claim owners.
        // Admin release and amendments also need current human project-admin
        // eligibility under the session directory, checked below (before the
        // replay lookup) and again in core.
        if matches!(
            &request,
            ActorRequest::AdminRelease { .. } | ActorRequest::Override(_)
        ) {
            let Principal::Human { user_id } = &verified else {
                return Err(ApiError::new(403, "auth.forbidden"));
            };
            if work
                .runtime
                .directory
                .project_role(&target.scope.project_id, user_id)
                != Some(ProjectRole::Admin)
            {
                return Err(ApiError::new(403, "auth.forbidden"));
            }
        }
        if let Some(identity) = &identity {
            let op = operation(&target, identity)?;
            let actor =
                control_principal(&Identity::from_principal(&verified, current.id.to_string()));
            if let Some(saved) = work
                .replayed::<Value>(&actor, replay.as_ref(), &op)
                .await
                .map_err(actor_error)?
            {
                return Ok(saved);
            }
        }
        execute_ready::<B>(
            &store,
            work,
            Identity::from_principal(&verified, current.id.to_string()),
            target,
            request,
            new_id,
            replay,
        )
        .await
    }
}
#[allow(clippy::too_many_arguments)]
pub async fn execute_ready<B: Backends>(
    store: &B::Execution,
    work: ActorWork<B::Execution>,
    identity: Identity,
    target: ActorTarget,
    request: ActorRequest,
    new_id: String,
    replay: Option<Replay>,
) -> Result<Value, ApiError> {
    match request {
        ActorRequest::Claim(request) => {
            let actor =
                declared_principal(&identity, Some(&request.session_id), request.actor.as_ref())?;
            let id = format!("clm_{new_id}")
                .parse()
                .map_err(|_| ApiError::new(422, "request.invalid"))?;
            json(
                claims::claim_step(work, &actor, step(&target)?, &request, id, replay.as_ref())
                    .await
                    .map_err(actor_error)?,
            )
        }
        ActorRequest::Heartbeat(request) => {
            let actor = claim_principal(&work, &identity, step(&target)?)?;
            drop(work); // lease functions acquire their own execution transaction
            json(
                claims::heartbeat(
                    store,
                    &actor,
                    &target.execution_id,
                    step(&target)?,
                    &request,
                    replay.as_ref(),
                )
                .await
                .map_err(actor_error)?,
            )
        }
        ActorRequest::Logs(request) => {
            let actor = claim_principal(&work, &identity, step(&target)?)?;
            drop(work);
            json(
                claims::append_logs(
                    store,
                    &actor,
                    &target.execution_id,
                    step(&target)?,
                    &request,
                    replay.as_ref(),
                )
                .await
                .map_err(actor_error)?,
            )
        }
        ActorRequest::Report(request) => {
            if &request.step_run_id != step(&target)? {
                return Err(ApiError::new(422, "request.step_mismatch"));
            }
            let actor = claim_principal(&work, &identity, step(&target)?)?;
            // UP1: `evd_` evidence entries name finalized uploads (D49).
            runbook_core::uploads::report_with_uploads(
                store,
                work,
                &actor,
                &request,
                replay.as_ref(),
            )
            .await
            .map_err(actor_error)?;
            Ok(Value::Null)
        }
        ActorRequest::Checklist(request) => {
            let actor = claim_principal(&work, &identity, step(&target)?)?;
            claims::update_checklist(work, &actor, step(&target)?, &request, replay.as_ref())
                .await
                .map_err(actor_error)?;
            Ok(Value::Null)
        }
        ActorRequest::Release(request) => {
            let actor = claim_principal(&work, &identity, step(&target)?)?;
            claims::release_claim(
                work,
                &actor,
                step(&target)?,
                &request.claim,
                replay.as_ref(),
            )
            .await
            .map_err(actor_error)?;
            Ok(Value::Null)
        }
        ActorRequest::AdminRelease { reason } => {
            let actor = declared_principal(&identity, None, None)?;
            claims::admin_release(work, &actor, step(&target)?, &reason, replay.as_ref())
                .await
                .map_err(actor_error)?;
            Ok(Value::Null)
        }
        ActorRequest::Decision(request) => {
            let actor = declared_principal(
                &identity,
                request.session_id.as_deref(),
                request.actor.as_ref(),
            )?;
            let response = if identity.human.is_some() {
                decisions::vote(work, &actor, step(&target)?, &request, replay.as_ref())
                    .await
                    .map_err(actor_error)?
            } else {
                let session = request
                    .session_id
                    .as_deref()
                    .ok_or_else(|| ApiError::new(422, "request.invalid"))?;
                decisions::agent_decide(
                    work,
                    &actor,
                    step(&target)?,
                    &request,
                    session,
                    replay.as_ref(),
                )
                .await
                .map_err(actor_error)?
            };
            json(response)
        }
        ActorRequest::Patch(request) => {
            let actor = control_principal(&identity);
            control::patch(work, &actor, &request, replay.as_ref())
                .await
                .map_err(actor_error)?;
            Ok(Value::Null)
        }
        ActorRequest::Override(request) => {
            // D35: only a human project admin amends; core rechecks the
            // in-session directory and replays a retried amendment once.
            if identity.human.is_none() {
                return Err(ApiError::new(403, "auth.forbidden"));
            }
            let actor = control_principal(&identity);
            let amendment =
                amendments::override_step(work, &actor, step(&target)?, &request, replay.as_ref())
                    .await
                    .map_err(actor_error)?;
            json(amendment)
        }
        ActorRequest::Signal(request) => {
            let actor = control_principal(&identity);
            let name = target
                .signal_name
                .as_deref()
                .ok_or_else(|| ApiError::new(422, "request.invalid"))?;
            control::signal(
                work,
                &actor,
                name,
                &format!("sig_{new_id}"),
                &request,
                replay.as_ref(),
            )
            .await
            .map_err(actor_error)?;
            Ok(Value::Null)
        }
    }
}

// Control payloads have no actor declaration. Preserve credential provenance
// without inventing a human signature for an agent or service principal.
pub fn control_principal(identity: &Identity) -> ActorPrincipal {
    let actor = if let Some(user_id) = &identity.human {
        ActorRef::Human {
            user_id: user_id.clone(),
        }
    } else {
        ActorRef::Agent {
            principal_id: identity.key.clone(),
            session_id: identity.credential_ref.clone(),
            name: identity.key.clone(),
            model: None,
        }
    };
    ActorPrincipal {
        principal: identity.key.clone(),
        actor,
        credential_ref: identity.credential_ref.clone(),
    }
}

#[cfg(test)]
mod closed_gate_tests {
    /// A decision after timeout handling owns the gate is a conflict with
    /// the gate's state, like `approval.not_open` (review F2).
    #[test]
    fn late_decision_codes_are_conflicts() {
        for code in ["approval.not_open", "approval.deadline_exceeded"] {
            assert_eq!(super::rejection(code, vec![]).status, 409, "{code}");
        }
    }
}
