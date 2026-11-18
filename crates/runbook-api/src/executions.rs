//! Public execution entry points. Authorization always precedes replay and data reads.
use crate::{Request, problem::Response};
use runbook_core::{catalog::TenantScope, credentials::*, directory_operations::*};
use runbook_model::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::future::Future;

#[derive(Debug)]
pub struct ExecutionApiError {
    pub status: u16,
    pub code: &'static str,
    pub diagnostics: Vec<Diagnostic>,
}
impl ExecutionApiError {
    pub fn new(status: u16, code: &'static str) -> Self {
        Self {
            status,
            code,
            diagnostics: vec![],
        }
    }
}
/// The durable adapter rechecks this identity inside the atomic start transaction.
#[derive(Clone, Debug)]
pub struct StartIdentity {
    pub key: String,
    pub principal: String,
    pub request_hash: [u8; 32],
}
/// A rerun of `original` (B9). The new run's data comes from the original's
/// start facts, never from the request body.
pub struct RerunInvocation {
    pub original: ExecutionId,
    pub execution_id: ExecutionId,
    pub version: Option<VersionSelector>,
    pub identity: StartIdentity,
    pub project_vars: Data,
    pub url: String,
}
pub struct StartInvocation {
    pub execution_id: ExecutionId,
    pub request: StartRequest,
    pub identity: StartIdentity,
    pub project_vars: Data,
    pub url: String,
}
/// Non-Send backend boundary. No HTTP-specific replay table: implementations use
/// the StartStore's atomic start/replay and its immutable initial outcome.
pub trait ExecutionBackend {
    fn search(
        &self,
        scope: &TenantScope,
        query: crate::search::SearchQuery,
    ) -> impl Future<Output = Result<Value, ExecutionApiError>>;

    fn execution_project(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> impl Future<Output = Result<Option<ProjectId>, ExecutionApiError>>;
    /// ARC2: whether a deleted run kept its summary row (retention deleted
    /// only its live detail; the run lives on in its daily archive).
    fn summary_retained(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> impl Future<Output = Result<bool, ExecutionApiError>>;
    /// Org-scoped lookup retained after execution_owner is removed.
    fn tombstone_project(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> impl Future<Output = Result<Option<ProjectId>, ExecutionApiError>>;
    fn find_replay(
        &self,
        scope: &TenantScope,
        identity: &StartIdentity,
    ) -> impl Future<Output = Result<Option<ExecutionView>, ExecutionApiError>>;
    fn start(
        &self,
        scope: &TenantScope,
        invocation: StartInvocation,
    ) -> impl Future<Output = Result<ExecutionView, ExecutionApiError>>;
    /// LANG9b: the latest published version of `runbook` (an ID or slug)
    /// and its triggers; `None` when it has no published version.
    fn published_triggers(
        &self,
        scope: &TenantScope,
        runbook: &str,
    ) -> impl Future<
        Output = Result<
            Option<(
                runbook_core::triggers::PublishedVersion,
                std::rc::Rc<runbook_spec::ir::Triggers>,
            )>,
            ExecutionApiError,
        >,
    >;
    fn read(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> impl Future<Output = Result<ExecutionView, ExecutionApiError>>;
    fn rerun(
        &self,
        scope: &TenantScope,
        invocation: RerunInvocation,
    ) -> impl Future<Output = Result<ExecutionView, ExecutionApiError>>;
    /// B8d: the live or terminal execution document from one revision.
    fn report(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> impl Future<Output = Result<ExecutionDocumentView, ExecutionApiError>>;
    /// The Markdown export of the same document (D34).
    fn report_markdown(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> impl Future<Output = Result<String, ExecutionApiError>>;
    /// REP2: the JUnit XML export of the same document, people named and
    /// the custom report only once terminal (D34).
    fn report_junit(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> impl Future<Output = Result<String, ExecutionApiError>>;
}
#[derive(Clone)]
pub struct ExecutionService<D, C, B> {
    pub directory: D,
    pub credentials: C,
    pub backend: B,
    pub origin: String,
}
/// A report download named after its run, `exe_…-report.<extension>`. The
/// id already parsed as an execution id; the filter keeps the header safe
/// whatever the path held.
fn report_attachment(
    path: &str,
    content_type: &'static str,
    extension: &str,
    bytes: Vec<u8>,
) -> crate::problem::Attachment {
    let id: String = crate::directory::match_path("/v1/executions/{exe}/report", path)
        .and_then(|params| params.get("exe").cloned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let stem = if id.is_empty() {
        "execution".into()
    } else {
        id
    };
    crate::problem::Attachment {
        content_type,
        filename: format!("{stem}-report.{extension}"),
        bytes,
        inline: false,
    }
}
pub fn recognizes(path: &str) -> bool {
    EXECUTION_ROUTES
        .iter()
        .any(|r| crate::directory::match_path(r.path, path).is_some())
}
impl<D: DirectoryOperations, C: CredentialStore, B: ExecutionBackend> ExecutionService<D, C, B> {
    pub async fn handle(&self, mut r: Request) -> Response {
        // LANG9b: a sender that can't set headers may put the token in the
        // trigger URL (`?token=`). Only this route reads it.
        if r.credential.is_none()
            && r.method == "POST"
            && crate::directory::match_path(TRIGGER_PATH, &r.path).is_some()
            && let Some(token) = r.query.get("token").filter(|t| !t.is_empty())
        {
            r.credential = Some(crate::Credential::Bearer(token.clone()));
        }
        let format = if r.path.ends_with("/report") {
            r.query.get("format").map(String::as_str)
        } else {
            None
        };
        match self.dispatch(&r).await {
            Ok((status, Value::String(text))) if format == Some("markdown") => {
                Response::markdown(status, text, &r.request_id)
            }
            // REP2: `junit` dispatches the XML, `pdf` the Markdown it renders.
            Ok((status, Value::String(text))) if format == Some("junit") => Response::attachment(
                status,
                report_attachment(&r.path, "application/xml", "xml", text.into_bytes()),
                &r.request_id,
            ),
            Ok((status, Value::String(text))) if format == Some("pdf") => Response::attachment(
                status,
                report_attachment(
                    &r.path,
                    "application/pdf",
                    "pdf",
                    runbook_report_pdf::render_pdf(&text),
                ),
                &r.request_id,
            ),
            Ok((status, view)) => Response::json(status, json!(view), &r.request_id),
            Err(e) => {
                let mut out = Response::error(e.status, e.code, &r.request_id);
                out.body["errors"] = json!(e.diagnostics);
                out
            }
        }
    }
    pub async fn authorize_events(
        &self,
        r: &Request,
    ) -> Result<(TenantScope, ExecutionId), ExecutionApiError> {
        let params = crate::directory::match_path("/v1/executions/{exe}/events", &r.path)
            .ok_or_else(|| ExecutionApiError::new(404, "resource.not_found"))?;
        let (scope, _) = self
            .authorize_target(r, &params, Scope::ExecutionsRead, true)
            .await?;
        let id = params["exe"]
            .parse()
            .map_err(|_| ExecutionApiError::new(404, "resource.not_found"))?;
        Ok((scope, id))
    }
    async fn authorize_target(
        &self,
        r: &Request,
        params: &std::collections::BTreeMap<String, String>,
        scope: Scope,
        include_tombstones: bool,
    ) -> Result<(TenantScope, Principal), ExecutionApiError> {
        let error = ExecutionApiError::new;
        let record = r.credential_record(&self.credentials).await.map_err(|e| {
            let (status, code) = e.status();
            error(status, code)
        })?;
        let now = self
            .directory
            .current_time_ms()
            .await
            .map_err(|_| error(503, "storage.unavailable"))?;
        if record.validate().is_err()
            || record.revoked_at.is_some()
            || record.expires_at.is_some_and(|t| t <= now)
        {
            return Err(error(401, "auth.invalid_credential"));
        }
        let snapshot = self
            .directory
            .snapshot(&record.org_id)
            .await
            .map_err(|_| error(503, "storage.unavailable"))?;
        let execution = params
            .get("exe")
            .map(|id| {
                id.parse::<ExecutionId>()
                    .map_err(|_| error(404, "resource.not_found"))
            })
            .transpose()?;
        let mut expired = false;
        let project = if let Some(execution) = &execution {
            let lookup_scope = TenantScope {
                org_id: record.org_id.clone(),
                project_id: snapshot
                    .projects
                    .keys()
                    .next()
                    .cloned()
                    .ok_or_else(|| error(404, "resource.not_found"))?,
            };
            match self
                .backend
                .execution_project(&lookup_scope, execution)
                .await?
            {
                Some(project) => project,
                None if include_tombstones => {
                    expired = true;
                    self.backend
                        .tombstone_project(&lookup_scope, execution)
                        .await?
                        .ok_or_else(|| error(404, "resource.not_found"))?
                }
                None => return Err(error(404, "resource.not_found")),
            }
        } else {
            let name = &params["p"];
            snapshot
                .projects
                .values()
                .find(|p| p.id.as_str() == name || p.slug == *name)
                .map(|p| p.id.clone())
                .ok_or_else(|| error(404, "resource.not_found"))?
        };
        let principal = authorize(&record, &snapshot, now, &project, scope).map_err(|e| {
            if expired {
                return error(404, "resource.not_found");
            }
            match e {
                AuthError::Forbidden => error(
                    403,
                    if record.scopes.contains(&scope) {
                        "auth.forbidden"
                    } else {
                        "auth.scope_missing"
                    },
                ),
                _ => error(401, "auth.invalid_credential"),
            }
        })?;
        self.credentials
            .touch_last_used(&record.org_id, &record.id, now)
            .await
            .map_err(|_| error(503, "storage.unavailable"))?;
        let tenant = TenantScope {
            org_id: record.org_id.clone(),
            project_id: project,
        };
        Ok((tenant, principal))
    }
    /// LANG9b: a call to a webhook trigger of the runbook's latest published
    /// version. The caller needs `executions:start` on the project, as for
    /// any start; the run records the trigger (`webhook:<id>`) as who
    /// started it, and the trigger's `dedupe` (else the call's
    /// `Idempotency-Key`) makes a repeated call return the same run.
    async fn webhook(
        &self,
        r: &Request,
        tenant: TenantScope,
        params: &std::collections::BTreeMap<String, String>,
    ) -> Result<(u16, Value), ExecutionApiError> {
        let error = ExecutionApiError::new;
        let trigger_id = &params["trigger"];
        let (published, triggers) = self
            .backend
            .published_triggers(&tenant, &params["rb"])
            .await?
            .ok_or_else(|| error(404, "resource.not_found"))?;
        let trigger = triggers
            .webhook
            .iter()
            .find(|t| t.id == *trigger_id)
            .ok_or_else(|| error(404, "trigger.not_found"))?;
        crate::field_caps::json("", &r.body, crate::field_caps::DATA_BYTES)?;
        let now = self
            .directory
            .current_time_ms()
            .await
            .map_err(|_| error(503, "storage.unavailable"))?;
        let (inputs, dedupe) = match runbook_engine::webhook_start(trigger, &r.body, now) {
            Ok(runbook_engine::WebhookOutcome::Start { inputs, dedupe }) => (inputs, dedupe),
            Ok(runbook_engine::WebhookOutcome::Skipped) => {
                return Ok((
                    200,
                    json!(TriggerResponse {
                        started: false,
                        duplicate: false,
                        reason: Some("when".into()),
                        execution: None,
                    }),
                ));
            }
            Err(runbook_engine::WebhookError::InvalidPayload(errors)) => {
                let mut refused = error(422, "trigger.invalid_payload");
                refused.diagnostics = errors
                    .into_iter()
                    .map(|e| Diagnostic {
                        path: e.path,
                        code: e.code,
                        message: e.message,
                        line: None,
                        column: None,
                    })
                    .collect();
                return Err(refused);
            }
            Err(runbook_engine::WebhookError::Evaluation(_)) => {
                return Err(error(422, "trigger.evaluation_failed"));
            }
        };
        let occasion = dedupe
            .or_else(|| r.idempotency_key.clone().filter(|k| !k.trim().is_empty()))
            .unwrap_or_else(|| r.request_id.clone());
        let key = runbook_core::triggers::start_key(
            "webhook",
            &published.runbook_id,
            trigger_id,
            &occasion,
        );
        let identity = StartIdentity {
            request_hash: Sha256::digest(key.as_bytes()).into(),
            key,
            principal: format!("webhook:{trigger_id}"),
        };
        if let Some(view) = self.backend.find_replay(&tenant, &identity).await? {
            return Ok((
                200,
                json!(TriggerResponse {
                    started: true,
                    duplicate: true,
                    reason: None,
                    execution: Some(view),
                }),
            ));
        }
        let variables = self
            .directory
            .project_variables(&tenant.org_id, &tenant.project_id)
            .await
            .map_err(|_| error(503, "storage.unavailable"))?;
        let execution_id: ExecutionId = format!("exe_{}", r.new_id)
            .parse()
            .map_err(|_| error(503, "server.invalid_id"))?;
        let invocation = StartInvocation {
            url: format!(
                "{}/v1/executions/{execution_id}",
                self.origin.trim_end_matches('/')
            ),
            execution_id,
            request: StartRequest {
                runbook: published.runbook_id.to_string(),
                version: Some(VersionSelector::Number(published.number)),
                inputs,
                fields: Data::new(),
                title: None,
                tags: vec![],
            },
            identity,
            project_vars: variables.variables.values,
        };
        let view = self.backend.start(&tenant, invocation).await?;
        Ok((
            201,
            json!(TriggerResponse {
                started: true,
                duplicate: false,
                reason: None,
                execution: Some(view),
            }),
        ))
    }
    /// B9: rerun after authorization on the original's project; the replay
    /// key is namespaced to this original so it never collides with starts.
    async fn rerun(
        &self,
        r: &Request,
        tenant: TenantScope,
        principal: Principal,
        original: ExecutionId,
    ) -> Result<(u16, Value), ExecutionApiError> {
        let error = ExecutionApiError::new;
        let identity = identity(r, &principal, &["execution.rerun", original.as_str()])?;
        if let Some(view) = self.backend.find_replay(&tenant, &identity).await? {
            return Ok((201, json!(view)));
        }
        let request: RerunRequest =
            serde_json::from_value(r.body.clone()).map_err(|_| error(422, "request.invalid"))?;
        let variables = self
            .directory
            .project_variables(&tenant.org_id, &tenant.project_id)
            .await
            .map_err(|_| error(503, "storage.unavailable"))?;
        let execution_id: ExecutionId = format!("exe_{}", r.new_id)
            .parse()
            .map_err(|_| error(503, "server.invalid_id"))?;
        let invocation = RerunInvocation {
            original,
            url: format!(
                "{}/v1/executions/{execution_id}",
                self.origin.trim_end_matches('/')
            ),
            execution_id,
            version: request.version,
            identity,
            project_vars: variables.variables.values,
        };
        Ok((201, json!(self.backend.rerun(&tenant, invocation).await?)))
    }
    async fn dispatch(&self, r: &Request) -> Result<(u16, Value), ExecutionApiError> {
        let error = ExecutionApiError::new;
        let (route, params) = EXECUTION_ROUTES
            .iter()
            .find_map(|route| {
                (route.method == r.method)
                    .then(|| crate::directory::match_path(route.path, &r.path).map(|p| (route, p)))
                    .flatten()
            })
            .ok_or_else(|| {
                if recognizes(&r.path) {
                    error(405, "request.method_not_allowed")
                } else {
                    error(404, "resource.not_found")
                }
            })?;
        let scope = if route.method == "POST" {
            Scope::ExecutionsStart
        } else {
            Scope::ExecutionsRead
        };
        // D33: report and rerun of a run past retention authorize against its
        // tombstone and answer 410; unauthorized callers still see 404.
        let tombstones = matches!(
            (route.method, route.path),
            ("GET", "/v1/executions/{exe}/report") | ("POST", "/v1/executions/{exe}/rerun")
        );
        let (tenant, principal) = self.authorize_target(r, &params, scope, tombstones).await?;
        if route.path == TRIGGER_PATH {
            return self.webhook(r, tenant, &params).await;
        }
        let execution = params
            .get("exe")
            .map(|s| {
                s.parse::<ExecutionId>()
                    .map_err(|_| error(404, "resource.not_found"))
            })
            .transpose()?;
        if let Some(execution) = execution {
            if tombstones
                && self
                    .backend
                    .execution_project(&tenant, &execution)
                    .await?
                    .is_none()
            {
                // ARC2 (PRD §14): a run reduced to its summary row by
                // retention has no inputs left to rerun.
                if route.method == "POST"
                    && self.backend.summary_retained(&tenant, &execution).await?
                {
                    return Err(error(422, "execution.inputs_expired"));
                }
                return Err(error(410, "execution.history_expired"));
            }
            return match (route.method, route.path) {
                ("GET", "/v1/executions/{exe}") => {
                    Ok((200, json!(self.backend.read(&tenant, &execution).await?)))
                }
                ("GET", "/v1/executions/{exe}/report") => {
                    match r.query.get("format").map(String::as_str) {
                        None | Some("json") => {
                            Ok((200, json!(self.backend.report(&tenant, &execution).await?)))
                        }
                        // `pdf` renders this same Markdown (D52) in `handle`.
                        Some("markdown" | "pdf") => Ok((
                            200,
                            Value::String(self.backend.report_markdown(&tenant, &execution).await?),
                        )),
                        Some("junit") => Ok((
                            200,
                            Value::String(self.backend.report_junit(&tenant, &execution).await?),
                        )),
                        Some(_) => Err(error(400, "request.invalid_query")),
                    }
                }
                ("POST", "/v1/executions/{exe}/rerun") => {
                    self.rerun(r, tenant, principal, execution).await
                }
                _ => Err(error(405, "request.method_not_allowed")),
            };
        }
        if route.method == "GET" {
            return Ok((
                200,
                self.backend
                    .search(&tenant, crate::search::parse_query(&r.query)?)
                    .await?,
            ));
        }
        let identity = identity(r, &principal, &["execution.start"])?;
        if let Some(view) = self.backend.find_replay(&tenant, &identity).await? {
            return Ok((201, json!(view)));
        }
        let request: StartRequest =
            serde_json::from_value(r.body.clone()).map_err(|_| error(422, "request.invalid"))?;
        // CF12b: the start record keeps this data whole, beside the
        // project's variables and the tags again.
        crate::field_caps::optional(
            "/title",
            request.title.as_deref(),
            crate::field_caps::TEXT_BYTES,
        )?;
        crate::field_caps::json(
            "",
            &(
                &request.inputs,
                &request.fields,
                &request.title,
                &request.tags,
            ),
            crate::field_caps::DATA_BYTES,
        )?;
        let variables = self
            .directory
            .project_variables(&tenant.org_id, &tenant.project_id)
            .await
            .map_err(|_| error(503, "storage.unavailable"))?;
        let execution_id: ExecutionId = format!("exe_{}", r.new_id)
            .parse()
            .map_err(|_| error(503, "server.invalid_id"))?;
        let invocation = StartInvocation {
            url: format!(
                "{}/v1/executions/{execution_id}",
                self.origin.trim_end_matches('/')
            ),
            execution_id,
            request,
            identity,
            project_vars: variables.variables.values,
        };
        Ok((201, json!(self.backend.start(&tenant, invocation).await?)))
    }
}

/// D20 start/rerun identity: namespaced by operation and the verified
/// principal, never the credential secret. The StartStore already binds the
/// organization and project. JSON supplies length framing, so a raw key can
/// never collide by injecting separators.
fn identity(
    r: &Request,
    principal: &Principal,
    namespace: &[&str],
) -> Result<StartIdentity, ExecutionApiError> {
    let error = ExecutionApiError::new;
    let key = r
        .idempotency_key
        .as_deref()
        .filter(|k| !k.trim().is_empty() && k.len() <= 256)
        .ok_or_else(|| error(428, "request.idempotency_key_required"))?;
    let principal = match principal {
        Principal::Human { user_id } => format!("human:{user_id}"),
        Principal::Agent { sponsor } => format!("agent:{sponsor}"),
        Principal::Service { token_id, .. } => format!("service:{token_id}"),
    };
    // Flat framing: ["execution.start", principal, key] stays byte-identical
    // to the B5b start key; reruns add the original ID to the namespace.
    let mut framed: Vec<Value> = namespace.iter().map(|part| json!(part)).collect();
    framed.push(json!(principal));
    framed.push(json!(key));
    let key_bytes = serde_json::to_vec(&framed).map_err(|_| error(422, "request.invalid"))?;
    let key = runbook_core::authoring::hex(&Sha256::digest(key_bytes));
    let payload = serde_json::to_vec(&r.body).map_err(|_| error(422, "request.invalid"))?;
    Ok(StartIdentity {
        key,
        principal,
        request_hash: Sha256::digest(payload).into(),
    })
}

/// The execution routes' mapping of a store error.
pub fn store_error(e: runbook_core::StoreError) -> ExecutionApiError {
    use runbook_core::StoreError;
    match e {
        StoreError::NotFound => ExecutionApiError::new(404, "resource.not_found"),
        StoreError::InvalidBatch("request.idempotency_key_reused") => {
            ExecutionApiError::new(409, "request.idempotency_key_reused")
        }
        // LANG3: `on_conflict: reject` refused the start (nothing written).
        StoreError::InvalidBatch(code @ runbook_core::concurrency::CONFLICT_CODE) => {
            ExecutionApiError::new(409, code)
        }
        // CF6 (PRD §17): over the organization's monthly quota.
        StoreError::InvalidBatch(code @ runbook_core::quotas::MONTHLY_CODE) => {
            ExecutionApiError::new(429, code)
        }
        // LANG4: a deletion that must wait for a call's result or cancel.
        StoreError::InvalidBatch(
            code @ (runbook_core::archives::job::RESULT_UNDELIVERED
            | runbook_core::archives::job::CHILD_CANCEL_PENDING),
        ) => ExecutionApiError::new(409, code),
        StoreError::InvalidBatch(code) => ExecutionApiError::new(422, code),
        StoreError::Backend(_) => ExecutionApiError::new(503, "storage.unavailable"),
    }
}

/// Rebuild a run's engine state from its pinned snapshot and event log,
/// checking both against the start record.
pub async fn restore_state(
    store: &impl runbook_core::blob::BlobStore,
    record: &runbook_core::start::StartRecord,
    run: &runbook_core::StoredExecution,
) -> Result<runbook_engine::ExecutionState, ExecutionApiError> {
    let bytes = store
        .get(&runbook_core::blob::BlobKey::Shared {
            hash: record.snapshot_digest,
        })
        .await
        .map_err(store_error)?;
    let compiled = runbook_spec::restore(&bytes)
        .map_err(|_| ExecutionApiError::new(503, "execution.snapshot_invalid"))?;
    if compiled.hash() != &record.semantic_hash || compiled.hash() != &run.projection.compiled_hash
    {
        return Err(ExecutionApiError::new(503, "execution.snapshot_invalid"));
    }
    let mut state = runbook_engine::ExecutionState::new(
        run.projection.execution_id.clone(),
        std::rc::Rc::new(compiled),
    );
    for event in &run.events {
        state = runbook_engine::apply(&state, event)
            .map_err(|_| ExecutionApiError::new(503, "execution.events_invalid"))?;
    }
    if state.sequence != run.revision {
        return Err(ExecutionApiError::new(503, "execution.events_invalid"));
    }
    Ok(state)
}
