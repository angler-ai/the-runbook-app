//! ARC3: daily archive routes (PRD §14; D53–D54). Portable: the host
//! supplies the tenant stores and the retention policy through
//! [`ArchiveBackend`].
//!
//! - `GET /v1/runbooks/{rb}/archives` lists the runbook's files, newest day
//!   first, within optional `from`/`to` dates; `format=zip` downloads
//!   `from..to` as one bundle, with `include_evidence=true` adding the
//!   evidence objects still held.
//! - `GET /v1/runbooks/{rb}/archives/{date}` downloads one day's file as
//!   stored, or a one-day bundle with `include_evidence=true`.
//! - `GET /v1/executions/{exe}/archive` opens one archived run read-only:
//!   its summary row and its folder, read by range through the ZIP index;
//!   `format=markdown` returns just its archived report.
//!
//! Every route needs `executions:read` on the runbook's or run's project; a
//! runbook or run outside the caller's reach is 404, never 403, so nothing
//! about another tenant's archives is disclosed.
use crate::{
    Request,
    problem::{Attachment, Response},
};
use runbook_core::{
    StoreError,
    archives::{
        ArchiveIndex, bundle, date_of,
        job::RetentionPolicy,
        parse_date,
        read::{self, record_key},
    },
    blob::BlobStore,
    catalog::TenantScope,
    credentials::{AuthError, CredentialStore, Scope, authorize},
    directory_operations::DirectoryOperations,
    retention::RunStore,
};
use runbook_model::*;
use serde_json::json;
use std::future::Future;

pub const LIST: &str = "/v1/runbooks/{rb}/archives";
pub const DAY: &str = "/v1/runbooks/{rb}/archives/{date}";
pub const RUN: &str = "/v1/executions/{exe}/archive";

pub fn recognizes(path: &str) -> bool {
    [LIST, DAY, RUN]
        .iter()
        .any(|pattern| crate::directory::match_path(pattern, path).is_some())
}

/// What the host provides.
pub trait ArchiveBackend {
    type Store: ArchiveIndex + BlobStore + RunStore;
    /// Org-scoped routing reads (they grant nothing): the project that owns
    /// a runbook, and the project of a run's summary row.
    fn runbook_project(
        &self,
        org: &OrganizationId,
        runbook_id: &RunbookId,
    ) -> impl Future<Output = Result<Option<ProjectId>, StoreError>>;
    fn archived_run_project(
        &self,
        org: &OrganizationId,
        execution_id: &ExecutionId,
    ) -> impl Future<Output = Result<Option<ProjectId>, StoreError>>;
    fn store(&self, scope: &TenantScope) -> Self::Store;
    /// The project's effective retention (D54), for `expires_at_ms`.
    fn policy(&self, project: &ProjectId) -> RetentionPolicy;
}

#[derive(Clone)]
pub struct ArchiveService<D, C, B> {
    pub directory: D,
    pub credentials: C,
    pub backend: B,
}

struct Failure(u16, &'static str);
fn not_found() -> Failure {
    Failure(404, "resource.not_found")
}
fn bad_query() -> Failure {
    Failure(400, "request.invalid_query")
}
/// D53/ARC3: stale days are not served until the job rewrites them.
pub const RETRY_AFTER_SECONDS: u16 = 900;
fn rewrite_pending() -> Failure {
    Failure(409, "archive.rewrite_pending")
}
fn storage(e: StoreError) -> Failure {
    match e {
        StoreError::NotFound => not_found(),
        StoreError::InvalidBatch("archive.rewrite_pending") => rewrite_pending(),
        StoreError::InvalidBatch(code) => Failure(422, code),
        StoreError::Backend(_) => Failure(503, "storage.unavailable"),
    }
}

enum Reply {
    Json(serde_json::Value),
    Text(String),
    File(Attachment),
}

fn flag(value: &str) -> Result<bool, Failure> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(bad_query()),
    }
}

/// A download name: ASCII letters, digits, `.`, `_`, `-` only.
fn file_name(runbook_id: &RunbookId, suffix: &str) -> String {
    format!("{runbook_id}-{suffix}")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect()
}

impl<D, C, B> ArchiveService<D, C, B>
where
    D: DirectoryOperations,
    C: CredentialStore,
    B: ArchiveBackend,
{
    pub async fn handle(&self, r: Request) -> Response {
        match self.dispatch(&r).await {
            Ok(Reply::Json(body)) => Response::json(200, body, &r.request_id),
            Ok(Reply::Text(text)) => Response::markdown(200, text, &r.request_id),
            Ok(Reply::File(file)) => Response::attachment(200, file, &r.request_id),
            Err(Failure(status, code)) => {
                let mut response = Response::error(status, code, &r.request_id);
                if code == "archive.rewrite_pending" {
                    response.retry_after = Some(RETRY_AFTER_SECONDS);
                }
                response
            }
        }
    }

    /// `executions:read` on `project`, else the caller learns nothing.
    async fn authorize(
        &self,
        r: &Request,
        project: impl AsyncFnOnce(&OrganizationId) -> Result<Option<ProjectId>, Failure>,
    ) -> Result<TenantScope, Failure> {
        let record = r.credential_record(&self.credentials).await.map_err(|e| {
            let (status, code) = e.status();
            Failure(status, code)
        })?;
        let now = self
            .directory
            .current_time_ms()
            .await
            .map_err(|_| Failure(503, "storage.unavailable"))?;
        if record.validate().is_err()
            || record.revoked_at.is_some()
            || record.expires_at.is_some_and(|t| t <= now)
        {
            return Err(Failure(401, "auth.invalid_credential"));
        }
        // The scope check needs no resource, so it discloses nothing.
        if !record.scopes.contains(&Scope::ExecutionsRead) {
            return Err(Failure(403, "auth.scope_missing"));
        }
        let project = project(&record.org_id).await?.ok_or_else(not_found)?;
        let snapshot = self
            .directory
            .snapshot(&record.org_id)
            .await
            .map_err(|_| Failure(503, "storage.unavailable"))?;
        authorize(&record, &snapshot, now, &project, Scope::ExecutionsRead).map_err(
            |e| match e {
                AuthError::Forbidden => not_found(),
                _ => Failure(401, "auth.invalid_credential"),
            },
        )?;
        self.credentials
            .touch_last_used(&record.org_id, &record.id, now)
            .await
            .map_err(|_| Failure(503, "storage.unavailable"))?;
        Ok(TenantScope {
            org_id: record.org_id,
            project_id: project,
        })
    }

    async fn dispatch(&self, r: &Request) -> Result<Reply, Failure> {
        let (route, params) = [LIST, DAY, RUN]
            .iter()
            .find_map(|p| crate::directory::match_path(p, &r.path).map(|params| (*p, params)))
            .ok_or_else(not_found)?;
        if r.method != "GET" {
            return Err(Failure(405, "request.method_not_allowed"));
        }
        if route == RUN {
            return self.run(r, &params["exe"]).await;
        }
        let runbook_id: RunbookId = params["rb"].parse().map_err(|_| not_found())?;
        let scope = self
            .authorize(r, async |org| {
                self.backend
                    .runbook_project(org, &runbook_id)
                    .await
                    .map_err(storage)
            })
            .await?;
        let store = self.backend.store(&scope);
        if route == DAY {
            // Checked after authorization: a bad date discloses nothing.
            let day = parse_date(&params["date"]).ok_or_else(not_found)?;
            let mut evidence = false;
            for (key, value) in &r.query {
                match key.as_str() {
                    "include_evidence" => evidence = flag(value)?,
                    _ => return Err(bad_query()),
                }
            }
            let suffix = format!("{}.zip", date_of(day));
            let bytes = if evidence {
                bundle::bundle(&store, &scope, &runbook_id, day, day, true)
                    .await
                    .map_err(storage)?
            } else {
                let record = store
                    .get_archive(&scope, &runbook_id, day)
                    .await
                    .map_err(storage)?
                    .ok_or_else(not_found)?;
                // A stale file may still hold a run deleted on request.
                if record.stale {
                    return Err(rewrite_pending());
                }
                store.get(&record_key(&record)).await.map_err(storage)?
            };
            return Ok(Reply::File(Attachment {
                content_type: "application/zip",
                filename: file_name(
                    &runbook_id,
                    &if evidence {
                        format!("{}-bundle.zip", date_of(day))
                    } else {
                        suffix
                    },
                ),
                bytes,
                inline: false,
            }));
        }
        let (mut from, mut to, mut zip, mut evidence) = (None, None, false, None);
        for (key, value) in &r.query {
            match key.as_str() {
                "from" => from = Some(parse_date(value).ok_or_else(bad_query)?),
                "to" => to = Some(parse_date(value).ok_or_else(bad_query)?),
                "format" => {
                    zip = match value.as_str() {
                        "json" => false,
                        "zip" => true,
                        _ => return Err(bad_query()),
                    }
                }
                "include_evidence" => evidence = Some(flag(value)?),
                _ => return Err(bad_query()),
            }
        }
        if from.zip(to).is_some_and(|(f, t)| f > t) || (evidence.is_some() && !zip) {
            return Err(bad_query());
        }
        if zip {
            let (Some(from), Some(to)) = (from, to) else {
                return Err(bad_query());
            };
            let bytes = bundle::bundle(
                &store,
                &scope,
                &runbook_id,
                from,
                to,
                evidence.unwrap_or(false),
            )
            .await
            .map_err(storage)?;
            return Ok(Reply::File(Attachment {
                content_type: "application/zip",
                filename: file_name(
                    &runbook_id,
                    &format!("{}-{}.zip", date_of(from), date_of(to)),
                ),
                bytes,
                inline: false,
            }));
        }
        let policy = self.backend.policy(&scope.project_id);
        let items = store
            .list_archives(&scope, &runbook_id, from, to)
            .await
            .map_err(storage)?
            .into_iter()
            .map(|record| ArchiveView {
                date: date_of(record.day),
                run_count: record.run_count,
                size_bytes: record.size_bytes,
                expires_at_ms: policy.archive_expiry_ms(record.day),
                stale: record.stale,
                sha256: runbook_core::authoring::hex(&record.digest),
                written_at_ms: record.written_at_ms,
            })
            .collect();
        Ok(Reply::Json(json!(ArchivePage { items })))
    }

    async fn run(&self, r: &Request, exe: &str) -> Result<Reply, Failure> {
        let execution_id: ExecutionId = exe.parse().map_err(|_| not_found())?;
        let scope = self
            .authorize(r, async |org| {
                self.backend
                    .archived_run_project(org, &execution_id)
                    .await
                    .map_err(storage)
            })
            .await?;
        let mut markdown = false;
        for (key, value) in &r.query {
            match (key.as_str(), value.as_str()) {
                ("format", "json") => markdown = false,
                ("format", "markdown") => markdown = true,
                _ => return Err(bad_query()),
            }
        }
        let store = self.backend.store(&scope);
        let opened = read::archived_run(&store, &scope, &execution_id)
            .await
            .map_err(storage)?
            .ok_or_else(not_found)?;
        // The summary row outlives its file (`summary_days`); once the file
        // expired the run's detail is gone.
        let (record, folder) = opened
            .file
            .ok_or(Failure(410, "execution.history_expired"))?;
        let report = read::report_markdown(&folder).map_err(storage)?;
        if markdown {
            return Ok(Reply::Text(report));
        }
        let live = store
            .tombstone(&execution_id)
            .await
            .map_err(storage)?
            .is_none();
        let summary = &opened.run.summary;
        let line = &folder.manifest;
        Ok(Reply::Json(json!(ArchivedRunView {
            execution_id: execution_id.clone(),
            runbook_id: opened.run.runbook_id.clone(),
            runbook_version_id: summary.version_id.clone(),
            runbook_version: summary.version,
            status: summary.status,
            title: summary.title.clone(),
            created_by: summary.created_by.clone(),
            created_at_ms: summary.created_at_ms,
            completed_at_ms: summary.completed_at_ms,
            date: date_of(opened.run.day),
            folder: opened.run.folder.clone(),
            live,
            stale: record.stale,
            files: line
                .files
                .iter()
                .map(|f| ArchivedFileView {
                    path: f.path.clone(),
                    bytes: f.bytes,
                    sha256: f.sha256.clone(),
                })
                .collect(),
            evidence: line
                .evidence
                .iter()
                .map(|e| ArchivedEvidenceView {
                    evd_id: e.evd_id.clone(),
                    step_run_id: e.step_run_id.clone(),
                    name: e.name.clone(),
                    content_type: e.content_type.clone(),
                    size: e.size,
                })
                .collect(),
            report_markdown: report,
        })))
    }
}
