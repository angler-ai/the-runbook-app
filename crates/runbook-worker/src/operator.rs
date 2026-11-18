//! CF8: the operator's project import and export on the api Worker, the
//! Cloudflare counterpart of `runbook-server import` and `export` (D3):
//! - `POST /internal/portable/import[?into_existing_org=true]`, a ZIP of the
//!   project archive's files (`manifest.json`, the NDJSON tables, `blobs/`);
//!   201 with the IDs and counts;
//! - `GET /internal/portable/export?org=<org_id>&project=<id or slug>`, the
//!   same ZIP.
//!
//! CF12: the import is one request under the Workers Paid limits. Its
//! archive is read whole, so its size is bounded by the isolate's 128 MB
//! ([`MAX_ARCHIVE_BYTES`], 413 `request.body_too_large`), and its writes by
//! the invocation's subrequests and D1's 30 seconds a batch
//! (`portable::ImportBudget`, 413 `import.too_large`); every refusal comes
//! before anything is written. One import runs at a time in an isolate
//! (409 `import.busy`; a hold with no progress for 15 minutes no longer
//! refuses: CF12b, and its review round 1). The export is refused (413 `export.too_large`)
//! before reading any run when its worst case does not fit the same budget.
//!
//! Both exist only with the `RUNBOOK_OPERATOR_SECRET` binding (a secret of
//! 32 or more characters); without it they are the API's 404. The secret
//! comes in `x-runbook-operator-secret`; a wrong one is 401, and a few
//! attempts a minute per isolate are allowed. The archive is the untrusted
//! input `runbook_core::portable::decode` verifies before anything is read.
use super::{CfBackends, WorkersHost, WorkersObjects, ensure_schema, nonce, to_response};
use crate::portable;
use runbook_api::backends::Backends;
use runbook_core::{credentials::secret_hash, portable::ImportOptions, portable::PortableError};
use runbook_model::{OrganizationId, ProjectId};
use runbook_store_d1::{D1, prepare, sql::Columns};
use serde_json::json;
use worker::{Env, Method, Request, Response, Result};

pub const SECRET_VAR: &str = "RUNBOOK_OPERATOR_SECRET";
pub const SECRET_HEADER: &str = "x-runbook-operator-secret";
pub const IMPORT: &str = "/internal/portable/import";
pub const EXPORT: &str = "/internal/portable/export";
const SECRET_MIN: usize = 32;
/// CF12: the largest archive an import reads, as sent and as its files
/// once inflated (past it a compressed archive is `import.too_large`). The
/// whole archive is in the isolate's memory, which is 128 MB on every
/// Workers plan. Measured locally (CF12): the wasm memory peaks at about
/// three times the archive when it is mostly blobs, and six times when it
/// is mostly step logs (each run's install is copied and encoded for its
/// object), plus 3.3 MB; the request and each object request are copied
/// on the JavaScript side too. At 10 MiB the worst case is about 84 MB
/// (CF12-3).
/// `cloudflare/tests/portable.test.mjs` imports archives of exactly this
/// size of both kinds and checks what they took.
pub const MAX_ARCHIVE_BYTES: u64 = 10 * 1_048_576;

type ApiResponse = runbook_api::problem::Response;

thread_local! {
    /// This isolate's import gate (CF12 review round 1; CF12b: a hold goes
    /// stale after `import_gate::STALE_AFTER_MS` without progress).
    static IMPORTING: crate::import_gate::Gate = const { crate::import_gate::Gate::new() };
}
/// This isolate's one running import; dropping it lets the next one in. A
/// request abandoned without being dropped holds the gate only until its
/// hold goes stale (CF12b).
struct Importing(u64);
impl Importing {
    fn begin(now_ms: i64) -> Option<Self> {
        IMPORTING.with(|gate| gate.begin(now_ms).map(Importing))
    }
    /// CF12b review round 1: this import made progress now.
    fn touch(&self) {
        let now = js_sys::Date::now() as i64;
        IMPORTING.with(|gate| gate.touch(self.0, now));
    }
}
impl Drop for Importing {
    fn drop(&mut self) {
        IMPORTING.with(|gate| gate.end(self.0));
    }
}

fn error(status: u16, code: &str, id: &str) -> Result<Response> {
    to_response(ApiResponse::error(status, code, id))
}

/// The status and code an import or export failure answers with.
fn failure(e: &PortableError) -> (u16, String) {
    match e {
        PortableError::Archive {
            code: portable::TOO_LARGE | portable::EXPORT_TOO_LARGE,
            ..
        } => (413, e.to_string()),
        PortableError::Archive { .. } => (422, e.to_string()),
        PortableError::TargetNotEmpty | PortableError::Conflict(_) => (409, e.to_string()),
        PortableError::Store(runbook_core::StoreError::NotFound) => {
            (404, "resource.not_found".into())
        }
        PortableError::Store(_) => (503, "storage.unavailable".into()),
    }
}

/// Serve one operator request (`IMPORT` or `EXPORT`).
pub async fn serve(mut req: Request, env: Env) -> Result<Response> {
    ensure_schema(&env).await?;
    let host = WorkersHost::new(&env, WorkersObjects::remote(&env)?)?;
    let id = nonce(&host)
        .map(|n| format!("req_{n}"))
        .unwrap_or_else(|_| "req_unavailable".into());
    let Some(expected) = env
        .secret(SECRET_VAR)
        .ok()
        .map(|s| s.to_string())
        .filter(|s| s.len() >= SECRET_MIN)
    else {
        return error(404, "resource.not_found", &id);
    };
    // Gate fix B4: compare first; only failures count (per client address,
    // shared by every isolate), so guesses cannot lock the operator out.
    let presented = req.headers().get(SECRET_HEADER)?;
    if !presented.is_some_and(|p| secret_hash(&p) == secret_hash(&expected)) {
        if let Err(retry) = super::secret_failed(&env, &req, "operator").await {
            return super::limited(retry);
        }
        return error(401, "auth.invalid_credential", &id);
    }
    let now = js_sys::Date::now() as i64;
    let url = req.url()?;
    let query: std::collections::BTreeMap<String, String> =
        url.query_pairs().into_owned().collect();
    let backends = CfBackends::new(host);
    match (req.method(), url.path()) {
        (Method::Post, IMPORT) => {
            // CF12 review round 1: one import at a time per isolate. Each
            // holds its archive and its decoded project; two would share the
            // 128 MB the memory bound was measured against.
            let Some(importing) = Importing::begin(now) else {
                return error(409, "import.busy", &id);
            };
            // CF12b review round 1: each chunk of the body, each stage and
            // each subrequest refreshes the hold.
            let progress = || importing.touch();
            let options = ImportOptions {
                into_existing_org: query.get("into_existing_org").is_some_and(|v| v == "true"),
            };
            // CF12: decided from the declared length before any byte is
            // read, or (chunked) once the body passes it.
            let crate::body::Capped::Body(bytes) =
                crate::body::read_watched(&mut req, MAX_ARCHIVE_BYTES, &progress).await?
            else {
                return error(413, "request.body_too_large", &id);
            };
            let data = match portable::from_zip_within(bytes, MAX_ARCHIVE_BYTES) {
                Ok(data) => data,
                Err(e) => {
                    let (status, code) = failure(&e);
                    return error(status, &code, &id);
                }
            };
            progress();
            let store = backends.execution(data.scope.clone());
            let budget = crate::seams::var(&env, "RUNBOOK_TEST_IMPORT_SUBREQUESTS")
                .and_then(|n| n.parse().ok())
                .map_or(portable::ImportBudget::PAID, |subrequests| {
                    portable::ImportBudget {
                        subrequests,
                        ..portable::ImportBudget::PAID
                    }
                });
            let imported =
                portable::import_watched(&store, &data, options, now, budget, &progress).await;
            let cost = match imported {
                Ok(cost) => cost,
                Err(e) => {
                    let (status, code) = failure(&e);
                    return error(status, &code, &id);
                }
            };
            // CF12: how much of its request's limits the import could have
            // spent (its worst case) and was allowed.
            let body = json!({
                "org_id": data.scope.org_id,
                "project_id": data.scope.project_id,
                "runbooks": data.rows.runbooks.len(),
                "runs": data.rows.runs.len(),
                "blobs": data.rows.blobs.len(),
                "subrequests": cost.subrequests,
                "subrequest_budget": budget.subrequests,
                "statements": cost.statements,
                "statement_budget": budget.batch_statements,
                "bound_values": cost.bound_values,
                "bound_bytes": cost.bound_bytes,
            });
            let mut response = Response::from_json(&body)?.with_status(201);
            // CF12 (test build only): the isolate's wasm memory after the
            // import, the high-water mark (linear memory never shrinks).
            if let Some(bytes) = crate::seams::wasm_memory_bytes() {
                response
                    .headers_mut()
                    .set("x-runbook-test-wasm-bytes", &bytes.to_string())?;
            }
            Ok(response)
        }
        (Method::Get, EXPORT) => {
            let Some(org) = query
                .get("org")
                .and_then(|o| o.parse::<OrganizationId>().ok())
            else {
                return error(400, "request.invalid", &id);
            };
            let Some(project) = query.get("project") else {
                return error(400, "request.invalid", &id);
            };
            // The project's slug is the directory's (the control database).
            let rows = backends
                .control()
                .all(
                    prepare(
                        "SELECT project_id FROM project WHERE org_id = ? \
                         AND (project_id = ? OR slug = ?)",
                    )
                    .bind(org.as_str())
                    .bind(project.as_str())
                    .bind(project.as_str()),
                )
                .await;
            let project_id: Option<ProjectId> = rows
                .ok()
                .and_then(|rows| rows.first().and_then(|row| row.text(0).ok()?.parse().ok()));
            let Some(project_id) = project_id else {
                return error(404, "export.project_not_found", &id);
            };
            let store = backends.execution(runbook_core::catalog::TenantScope {
                org_id: org,
                project_id,
            });
            let zip = match portable::export(&store)
                .await
                .and_then(|data| portable::to_zip(&data, now))
            {
                Ok(zip) => zip,
                Err(e) => {
                    let (status, code) = failure(&e);
                    return error(status, &code, &id);
                }
            };
            let mut response = Response::from_bytes(zip)?;
            let headers = response.headers_mut();
            headers.set("content-type", "application/zip")?;
            headers.set(
                "content-disposition",
                "attachment; filename=\"project-archive.zip\"",
            )?;
            headers.set("cache-control", "no-store")?;
            Ok(response)
        }
        _ => error(405, "request.method_not_allowed", &id),
    }
}
