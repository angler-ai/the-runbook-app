//! Public authoring routes. Sources and bundles never enter a write transaction
//! until validation/compilation and immutable blob storage have completed.
use crate::{Request, problem::Response};
use runbook_core::{
    StoreError,
    authoring::*,
    blob::{BlobKey, BlobStore},
    catalog::*,
    credentials::*,
    directory::*,
    directory_operations::*,
};
use runbook_model::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
mod context;
mod reads;
pub use context::validate;
use context::*;
type Error = Response;
pub fn recognizes(path: &str) -> bool {
    AUTHORING_ROUTES
        .iter()
        .any(|route| crate::directory::match_path(route.path, path).is_some())
}
#[derive(Clone, Debug)]
pub struct AuthoringLimits {
    pub source_bytes: usize,
    pub skill_bytes: u64,
}
impl Default for AuthoringLimits {
    fn default() -> Self {
        Self {
            source_bytes: 1_048_576,
            skill_bytes: u64::MAX,
        }
    }
}
#[derive(Clone)]
pub struct AuthoringService<D, C, F> {
    pub directory: D,
    pub credentials: C,
    pub factory: F,
    pub limits: AuthoringLimits,
}
impl<D: DirectoryOperations, C: CredentialStore, F: AuthoringFactory> AuthoringService<D, C, F> {
    pub fn new(directory: D, credentials: C, factory: F, limits: AuthoringLimits) -> Self {
        Self {
            directory,
            credentials,
            factory,
            limits,
        }
    }
    pub async fn handle(&self, r: Request) -> Response {
        match self.dispatch(&r).await {
            Ok(v) => {
                let mut response = Response::json(v.status, v.body, &r.request_id);
                response.etag = v.etag;
                response
            }
            Err(e) => e,
        }
    }
    async fn dispatch(&self, r: &Request) -> Result<StoredResponse, Error> {
        let (route, p) = AUTHORING_ROUTES
            .iter()
            .find_map(|route| {
                (route.method == r.method)
                    .then(|| crate::directory::match_path(route.path, &r.path).map(|p| (route, p)))
                    .flatten()
            })
            .ok_or_else(|| {
                if recognizes(&r.path) {
                    error(r, 405, "request.method_not_allowed")
                } else {
                    error(r, 404, "resource.not_found")
                }
            })?;
        for _ in 0..4 {
            let record = r.credential_record(&self.credentials).await.map_err(|e| {
                let (status, code) = e.status();
                error(r, status, code)
            })?;
            if record.revoked_at.is_some() {
                return Err(error(r, 401, "auth.invalid_credential"));
            }
            let s = self
                .directory
                .snapshot(&record.org_id)
                .await
                .map_err(|_| error(r, 503, "storage.unavailable"))?;
            let now = self
                .directory
                .current_time_ms()
                .await
                .map_err(|_| error(r, 503, "storage.unavailable"))?;
            if record.expires_at.is_some_and(|expiry| expiry <= now) {
                return Err(error(r, 401, "auth.invalid_credential"));
            }
            let rb = p.get("rb").map(|v| parse_id(r, v)).transpose()?;
            let project = if let Some(v) = p.get("p") {
                Some(resolve_project(r, &s, v)?)
            } else if let Some(rb) = &rb {
                Some(
                    self.factory
                        .project_for_runbook(&s.org_id, rb)
                        .await
                        .map_err(|e| store_error(r, e))?
                        .ok_or_else(|| error(r, 404, "resource.not_found"))?,
                )
            } else if let Some(v) = r.body.get("project").and_then(Value::as_str) {
                Some(resolve_project(r, &s, v)?)
            } else {
                None
            };
            let scope = if route.path.contains("/skills") {
                if r.method == "GET" {
                    Scope::SkillsRead
                } else {
                    Scope::SkillsWrite
                }
            } else if r.method == "GET" || route.path == "/v1/validate" && project.is_some() {
                Scope::RunbooksRead
            } else if route.path.ends_with("/versions") {
                Scope::RunbooksPublish
            } else {
                Scope::RunbooksWrite
            };
            let principal = if let Some(project) = &project {
                authorize_scope(r, &record, &s, now, project, scope)?
            } else {
                // Contextless validation still requires a current authoring/read credential.
                let mut allowed = None;
                for project in s.projects.keys() {
                    for scope in [Scope::RunbooksWrite, Scope::RunbooksRead] {
                        if let Ok(p) = authorize_scope(r, &record, &s, now, project, scope) {
                            allowed = Some(p);
                            break;
                        }
                    }
                    if allowed.is_some() {
                        break;
                    }
                }
                allowed.ok_or_else(|| error(r, 403, "auth.forbidden"))?
            };
            self.credentials
                .touch_last_used(&s.org_id, &record.id, now)
                .await
                .map_err(|_| error(r, 503, "storage.unavailable"))?;
            let principal = principal_key(principal);
            let store = project.as_ref().map(|project| {
                self.factory.scoped(TenantScope {
                    org_id: s.org_id.clone(),
                    project_id: project.clone(),
                })
            });
            if route.path == "/v1/validate" {
                let b: ValidateRequest = parse(r, &r.body)?;
                self.source_limit(r, &b.source)?;
                let context = if let (Some(project), Some(store)) = (&project, &store) {
                    self.compile_context(r, &s, project, store, &b.source)
                        .await?
                } else {
                    runbook_spec::CompileContext::default()
                };
                return Ok(response(200, json!(validate(&b.source, &context))));
            }
            let store = store.ok_or_else(|| error(r, 404, "resource.not_found"))?;
            let catalog = store.catalog();
            if r.method == "GET" {
                // UI-1 review: the runbook views' run fields (`last_run`,
                // `active_runs`) are run data, which every other read
                // gates on executions:read; without it they are omitted.
                let with_runs = project.as_ref().is_some_and(|project| {
                    authorize(&record, &s, now, project, Scope::ExecutionsRead).is_ok()
                });
                return self
                    .read(r, route, &p, rb.as_ref(), &store, &principal, with_runs)
                    .await;
            }
            let replay = if r.method == "POST" {
                let key = r
                    .idempotency_key
                    .as_deref()
                    .filter(|k| !k.trim().is_empty() && k.len() <= 256)
                    .ok_or_else(|| error(r, 428, "request.idempotency_key_required"))?;
                let replay = RequestReplay {
                    principal,
                    operation: format!(
                        "{}:{}:{}:{}",
                        r.method,
                        route.path,
                        json!(p),
                        project.as_ref().expect("project selected")
                    ),
                    key: key.into(),
                    payload: json!({"body":r.body,"if_match":r.if_match}),
                };
                match self
                    .directory
                    .lookup_replay(&s.org_id, s.revision, &replay)
                    .await
                    .map_err(|_| error(r, 503, "storage.unavailable"))?
                {
                    ReplayResult::Applied(v) => return Ok(v),
                    ReplayResult::KeyReused => {
                        return Err(error(r, 409, "request.idempotency_key_reused"));
                    }
                    ReplayResult::Conflict => continue,
                    ReplayResult::Missing => {}
                }
                Some(replay)
            } else {
                None
            };
            let command = match route.path {
                "/v1/projects/{p}/runbooks" => {
                    let b: CreateRunbookRequest = parse(r, &r.body)?;
                    self.source_limit(r, &b.source)?;
                    let slug = creation_slug(r, &b)?;
                    let context = self
                        .compile_context(
                            r,
                            &s,
                            project
                                .as_ref()
                                .ok_or_else(|| error(r, 404, "resource.not_found"))?,
                            &store,
                            &b.source,
                        )
                        .await?;
                    AuthoringCommand::Create {
                        record: RunbookRecord {
                            runbook_id: parse_id(r, &format!("rb_{}", r.new_id))?,
                            slug,
                            origin: RunbookOrigin::Ui,
                            archived: false,
                        },
                        diagnostics: runbook_core::publish::draft_diagnostics(&b.source, &context),
                        source: b.source,
                    }
                }
                "/v1/runbooks/{rb}/draft" | "/v1/runbooks/{rb}/versions" => {
                    let rb = rb
                        .as_ref()
                        .ok_or_else(|| error(r, 404, "resource.not_found"))?;
                    let record = catalog
                        .read_runbook(rb)
                        .await
                        .map_err(|e| store_error(r, e))?
                        .ok_or_else(|| error(r, 404, "resource.not_found"))?;
                    if record.origin == RunbookOrigin::Git {
                        return Err(error(r, 409, "runbook.git_managed"));
                    }
                    let draft = catalog
                        .get_draft(rb)
                        .await
                        .map_err(|e| store_error(r, e))?
                        .ok_or_else(|| error(r, 404, "resource.not_found"))?;
                    let supplied = r
                        .if_match
                        .as_deref()
                        .ok_or_else(|| error(r, 428, "request.if_match_required"))?;
                    if supplied != draft_etag(&draft) {
                        return Err(error(r, 412, "catalog.draft_conflict"));
                    }
                    if r.method == "PATCH" {
                        let b: SaveDraftRequest = parse(r, &r.body)?;
                        self.source_limit(r, &b.source)?;
                        let context = self
                            .compile_context(
                                r,
                                &s,
                                project
                                    .as_ref()
                                    .ok_or_else(|| error(r, 404, "resource.not_found"))?,
                                &store,
                                &b.source,
                            )
                            .await?;
                        AuthoringCommand::Save {
                            runbook_id: rb.clone(),
                            revision: draft.revision.get(),
                            diagnostics: runbook_core::publish::draft_diagnostics(
                                &b.source, &context,
                            ),
                            source: b.source,
                        }
                    } else {
                        if !r.body.is_null() && r.body != json!({}) {
                            return Err(error(r, 422, "request.invalid"));
                        }
                        let context = self
                            .compile_context(
                                r,
                                &s,
                                project
                                    .as_ref()
                                    .ok_or_else(|| error(r, 404, "resource.not_found"))?,
                                &store,
                                &draft.source,
                            )
                            .await?;
                        let prepared = runbook_core::publish::prepare(
                            &record.slug,
                            &draft.source,
                            &context,
                            parse_id(r, &format!("rbv_{}", r.new_id))?,
                        )
                        .map_err(|errors| diagnostics_error(r, errors))?;
                        catalog
                            .put(
                                BlobKey::Shared {
                                    hash: prepared.snapshot_digest,
                                },
                                &prepared.snapshot,
                            )
                            .await
                            .map_err(|e| store_error(r, e))?;
                        AuthoringCommand::Publish {
                            runbook_id: rb.clone(),
                            revision: draft.revision.get(),
                            prepared: Box::new(prepared),
                        }
                    }
                }
                "/v1/projects/{p}/skills" => {
                    use base64::Engine;
                    let b: SkillUploadRequest = parse(r, &r.body)?;
                    // CF12b: the manifest (a line a file) and the reference
                    // are each kept whole in one row.
                    let too_large = |_| error(r, 422, crate::field_caps::CODE);
                    if b.files.len() > crate::field_caps::SKILL_FILES {
                        return Err(error(r, 422, crate::field_caps::CODE));
                    }
                    crate::field_caps::optional(
                        "/reference",
                        b.reference.as_deref(),
                        crate::field_caps::TEXT_BYTES,
                    )
                    .map_err(too_large)?;
                    let files = b
                        .files
                        .into_iter()
                        .map(|f| {
                            Ok(runbook_core::skills::SkillFile {
                                path: f.path,
                                content: base64::engine::general_purpose::STANDARD
                                    .decode(f.content)
                                    .map_err(|_| error(r, 422, "skill.invalid_base64"))?,
                            })
                        })
                        .collect::<Result<Vec<_>, Error>>()?;
                    let provenance = SkillProvenance {
                        reference: b.reference,
                        label: format!("credential:{}", record.id),
                    };
                    let version = runbook_core::skill_library::prepare_skill(
                        catalog,
                        &files,
                        provenance,
                        self.limits.skill_bytes,
                    )
                    .await
                    .map_err(|e| match e {
                        runbook_core::skill_library::PublishSkillError::Bundle(e) => error(
                            r,
                            if matches!(e, runbook_core::skills::SkillError::TooLarge) {
                                413
                            } else {
                                422
                            },
                            e.code(),
                        ),
                        runbook_core::skill_library::PublishSkillError::Store(e) => {
                            store_error(r, e)
                        }
                    })?;
                    AuthoringCommand::Skill { version }
                }
                _ => return Err(error(r, 404, "resource.not_found")),
            };
            match store.commit_authoring(s.revision, replay, command).await {
                Err(StoreError::InvalidBatch("directory.concurrent_change")) => continue,
                v => return v.map_err(|e| store_error(r, e)),
            }
        }
        Err(error(r, 409, "directory.concurrent_change"))
    }
    fn source_limit(&self, r: &Request, source: &str) -> Result<(), Error> {
        if source.len()
            > self
                .limits
                .source_bytes
                .min(runbook_spec::authoring::MAX_SOURCE_BYTES)
        {
            Err(error(r, 413, "spec.source_limit"))
        } else {
            Ok(())
        }
    }
}
fn error(r: &Request, status: u16, code: &str) -> Error {
    Response::error(status, code, &r.request_id)
}
fn parse<T: serde::de::DeserializeOwned>(r: &Request, v: &Value) -> Result<T, Error> {
    serde_json::from_value(v.clone()).map_err(|_| error(r, 422, "request.invalid"))
}
fn parse_id<T: std::str::FromStr>(r: &Request, v: &str) -> Result<T, Error> {
    v.parse().map_err(|_| error(r, 404, "resource.not_found"))
}
fn response(status: u16, body: Value) -> StoredResponse {
    StoredResponse {
        status,
        body,
        etag: None,
    }
}
fn store_error(r: &Request, e: StoreError) -> Error {
    match e {
        StoreError::NotFound => error(r, 404, "resource.not_found"),
        StoreError::InvalidBatch(code) => error(
            r,
            match code {
                "catalog.draft_conflict" => 412,
                "catalog.invalid_slug" => 422,
                _ => 409,
            },
            code,
        ),
        _ => error(r, 503, "storage.unavailable"),
    }
}
fn diagnostics_error(r: &Request, errors: Vec<Diagnostic>) -> Error {
    let code = errors
        .first()
        .map(|d| d.code.as_str())
        .unwrap_or("spec.invalid");
    let mut e = error(r, 422, code);
    e.body["errors"] = json!(errors);
    e
}
fn authorize_scope(
    r: &Request,
    record: &CredentialRecord,
    s: &DirectorySnapshot,
    now: i64,
    p: &ProjectId,
    scope: Scope,
) -> Result<Principal, Error> {
    authorize(record, s, now, p, scope).map_err(|e| {
        let mut response = match e {
            AuthError::Forbidden => error(
                r,
                403,
                if record.scopes.contains(&scope) {
                    "auth.forbidden"
                } else {
                    "auth.scope_missing"
                },
            ),
            _ => error(r, 401, "auth.invalid_credential"),
        };
        if scope == Scope::RunbooksPublish && response.status == 403 {
            response.body["next_action"] = json!("ask_human_to_publish");
        }
        response
    })
}
fn principal_key(p: Principal) -> String {
    match p {
        Principal::Human { user_id } => format!("human:{user_id}"),
        Principal::Agent { sponsor } => format!("agent:{sponsor}"),
        Principal::Service { token_id, .. } => format!("service:{token_id}"),
    }
}
fn resolve_project(r: &Request, s: &DirectorySnapshot, v: &str) -> Result<ProjectId, Error> {
    s.projects
        .values()
        .find(|p| p.id.as_str() == v || p.slug == v)
        .map(|p| p.id.clone())
        .ok_or_else(|| error(r, 404, "resource.not_found"))
}
