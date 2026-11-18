//! B6b transport-independent actor routes. Backends own atomic state + replay.
pub use crate::executions::ExecutionApiError as ApiError;
use crate::{
    Request,
    actor_requests::{ActorReplay, ActorRequest, admin_release_reason, decode, post_identity},
    problem::Response,
};
use runbook_core::{
    catalog::TenantScope, credentials::*, directory_operations::DirectoryOperations,
};
use runbook_model::*;
use serde_json::Value;
use std::future::Future;

pub struct ActorTarget {
    pub operation: &'static str,
    pub scope: TenantScope,
    pub execution_id: ExecutionId,
    pub step_run_id: Option<StepRunId>,
    pub signal_name: Option<String>,
}
// Verified credential facts, never a principal copied from a request body.
pub struct ActorInvocation {
    pub target: ActorTarget,
    pub principal: Principal,
    pub credential: CredentialRecord,
    pub replay: Option<ActorReplay>,
    pub request: ActorRequest,
    pub new_id: String,
}
pub trait ActorBackend {
    fn step_run_execution(
        &self,
        org: &OrganizationId,
        step: &StepRunId,
    ) -> impl Future<Output = Result<Option<(ExecutionId, ProjectId)>, ApiError>>;
    fn execution_project(
        &self,
        org: &OrganizationId,
        execution: &ExecutionId,
    ) -> impl Future<Output = Result<Option<ProjectId>, ApiError>>;
    /// Read only; execution must repeat lookup atomically before evaluating state.
    fn find_replay(
        &self,
        target: &ActorTarget,
        replay: &ActorReplay,
    ) -> impl Future<Output = Result<Option<Value>, ApiError>>;
    /// Recheck current credential/access and in-session directory eligibility;
    /// persist success and replay in the same transaction. No HTTP replay write.
    fn execute(&self, invocation: ActorInvocation)
    -> impl Future<Output = Result<Value, ApiError>>;
}
#[derive(Clone)]
pub struct ActorService<D, C, B> {
    pub directory: D,
    pub credentials: C,
    pub backend: B,
    pub evidence_schemes: Vec<String>,
}
pub const ROUTES: &[(&str, &str, &str)] = &[
    ("POST", "/v1/step-runs/{sr}/claim", "claim"),
    ("POST", "/v1/step-runs/{sr}/heartbeat", "heartbeat"),
    ("POST", "/v1/step-runs/{sr}/logs", "logs"),
    ("POST", "/v1/step-runs/{sr}/report", "report"),
    ("POST", "/v1/step-runs/{sr}/checklist", "checklist"),
    ("POST", "/v1/step-runs/{sr}/release", "release"),
    ("POST", "/v1/step-runs/{sr}/decisions", "decision"),
    ("POST", "/v1/step-runs/{sr}/overrides", "override"),
    ("PATCH", "/v1/executions/{exe}", "patch"),
    ("POST", "/v1/executions/{exe}/signals/{name}", "signal"),
];
/// F56: deprecated aliases, served like their route but absent from the
/// contract. `/decision` is kept for CLI and MCP builds released before
/// PRD section 9's `/decisions`.
pub const DEPRECATED_ROUTES: &[(&str, &str, &str)] =
    &[("POST", "/v1/step-runs/{sr}/decision", "decision")];
/// Every served actor route, the contract's first.
pub fn served_routes() -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str)>
{
    ROUTES.iter().chain(DEPRECATED_ROUTES)
}
impl<D: DirectoryOperations, C: CredentialStore, B: ActorBackend> ActorService<D, C, B> {
    pub async fn handle(&self, r: Request) -> Response {
        match self.dispatch(&r).await {
            Ok(value) => Response::json(200, value, &r.request_id),
            Err(e) => {
                let mut out = Response::error(e.status, e.code, &r.request_id);
                out.body["errors"] = serde_json::json!(e.diagnostics);
                out
            }
        }
    }
    async fn dispatch(&self, r: &Request) -> Result<Value, ApiError> {
        let err = ApiError::new;
        let (method, op, params) = served_routes()
            .find_map(|(method, path, op)| {
                crate::directory::match_path(path, &r.path).map(|p| (*method, *op, p))
            })
            .ok_or_else(|| err(404, "resource.not_found"))?;
        if r.method != method {
            return Err(err(405, "request.method_not_allowed"));
        }
        let record = r.credential_record(&self.credentials).await.map_err(|e| {
            let (status, code) = e.status();
            err(status, code)
        })?;
        let now = self
            .directory
            .current_time_ms()
            .await
            .map_err(|_| err(503, "storage.unavailable"))?;
        if record.validate().is_err()
            || record.revoked_at.is_some()
            || record.expires_at.is_some_and(|t| t <= now)
        {
            return Err(err(401, "auth.invalid_credential"));
        }
        let (execution_id, project_id, step_run_id) = if let Some(sr) = params.get("sr") {
            let sr: StepRunId = sr.parse().map_err(|_| err(404, "resource.not_found"))?;
            let (exe, project) = self
                .backend
                .step_run_execution(&record.org_id, &sr)
                .await?
                .ok_or_else(|| err(404, "resource.not_found"))?;
            (exe, project, Some(sr))
        } else {
            let exe: ExecutionId = params["exe"]
                .parse()
                .map_err(|_| err(404, "resource.not_found"))?;
            let project = self
                .backend
                .execution_project(&record.org_id, &exe)
                .await?
                .ok_or_else(|| err(404, "resource.not_found"))?;
            (exe, project, None)
        };
        let snapshot = self
            .directory
            .snapshot(&record.org_id)
            .await
            .map_err(|_| err(503, "storage.unavailable"))?;
        let scope = if matches!(op, "patch" | "override") {
            Scope::ExecutionsAdmin
        } else {
            Scope::ExecutionsAct
        };
        let principal =
            authorize(&record, &snapshot, now, &project_id, scope).map_err(|e| match e {
                AuthError::Forbidden => err(
                    403,
                    if record.scopes.contains(&scope) {
                        "auth.forbidden"
                    } else {
                        "auth.scope_missing"
                    },
                ),
                _ => err(401, "auth.invalid_credential"),
            })?;
        let admin_release = op == "release" && r.body.get("claim").is_none();
        // Admin-only operations recheck the human project-admin role here,
        // before any replay lookup (D20: authorization precedes replay).
        if admin_release || op == "override" {
            let allowed = match &principal {
                Principal::Human { user_id } => {
                    snapshot.project_role(&project_id, user_id) == Some(ProjectRole::Admin)
                }
                _ => false,
            };
            if !allowed {
                return Err(err(403, "auth.forbidden"));
            }
        }
        self.credentials
            .touch_last_used(&record.org_id, &record.id, now)
            .await
            .map_err(|_| err(503, "storage.unavailable"))?;
        let principal_key = match &principal {
            Principal::Human { user_id } => format!("human:{user_id}"),
            Principal::Agent { sponsor } => format!("agent:{sponsor}"),
            Principal::Service { token_id, .. } => format!("service:{token_id}"),
        };
        let target = ActorTarget {
            operation: if admin_release { "admin_release" } else { op },
            scope: TenantScope {
                org_id: record.org_id.clone(),
                project_id,
            },
            execution_id,
            step_run_id,
            signal_name: params.get("name").cloned(),
        };
        let replay = if method == "POST" {
            Some(post_identity(r, &principal_key)?)
        } else {
            None
        };
        if let Some(replay) = &replay
            && let Some(saved) = self.backend.find_replay(&target, replay).await?
        {
            return Ok(saved);
        }
        let schemes = self
            .evidence_schemes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let request = if admin_release {
            ActorRequest::AdminRelease {
                reason: admin_release_reason(&r.body)?.to_owned(),
            }
        } else {
            decode(
                op,
                target.step_run_id.as_ref().map_or("", StepRunId::as_str),
                r.body.clone(),
                &schemes,
            )?
        };
        let kind = if matches!(principal, Principal::Human { .. }) {
            runbook_model::ActorKind::Human
        } else {
            runbook_model::ActorKind::Agent
        };
        match &request {
            ActorRequest::Claim(r) => crate::actor_requests::validate_declaration(
                kind,
                Some(&r.session_id),
                r.actor.as_ref(),
                true,
            )?,
            ActorRequest::Decision(r) => crate::actor_requests::validate_declaration(
                kind,
                r.session_id.as_deref(),
                r.actor.as_ref(),
                false,
            )?,
            _ => (),
        }
        self.backend
            .execute(ActorInvocation {
                target,
                principal,
                credential: record,
                replay,
                request,
                new_id: r.new_id.clone(),
            })
            .await
    }
}

pub fn recognizes(method: &str, path: &str) -> bool {
    served_routes().any(|(_, pattern, op)| {
        (*op != "patch" || method == "PATCH")
            && crate::directory::match_path(pattern, path).is_some()
    })
}
