//! SLACK1 stage 1: a project's Slack incoming-webhook URL. The URL is
//! a credential: it is written with project-admin authority, never read
//! back (reads say only whether one is configured), and never logged.
use crate::{Request, backends::Backends, problem::Response};
use runbook_core::{
    credentials::{Scope, authorize},
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    notify::NotifyStore,
};
use runbook_model::{ProjectSlackRequest, ProjectSlackView};
use serde_json::json;

type Failure = (u16, &'static str);
const UNAVAILABLE: Failure = (503, "storage.unavailable");

pub fn recognizes(path: &str) -> bool {
    crate::directory::match_path("/v1/projects/{p}/slack", path).is_some()
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok((status, body)) => Response::json(status, body, &r.request_id),
        Err((status, code)) => Response::error(status, code, &r.request_id),
    }
}

async fn dispatch<B: Backends>(
    backends: &B,
    r: &Request,
) -> Result<(u16, serde_json::Value), Failure> {
    let params = crate::directory::match_path("/v1/projects/{p}/slack", &r.path)
        .ok_or((404, "resource.not_found"))?;
    let project_ref = params.get("p").ok_or((404, "resource.not_found"))?;
    let credentials = backends.credentials();
    let directory = backends.directory();
    let record = r
        .credential_record(&credentials)
        .await
        .map_err(|e| e.status())?;
    let now = directory.current_time_ms().await.map_err(|_| UNAVAILABLE)?;
    let snapshot = directory
        .snapshot(&record.org_id)
        .await
        .map_err(|_| UNAVAILABLE)?;
    let project_id = snapshot
        .projects
        .values()
        .find(|project| project.slug == *project_ref || project.id.as_str() == project_ref)
        .map(|project| project.id.clone())
        .ok_or((404, "resource.not_found"))?;
    let scope = runbook_core::catalog::TenantScope {
        org_id: record.org_id.clone(),
        project_id,
    };
    let notify = backends.notify();
    match r.method.as_str() {
        "GET" => {
            authorize(
                &record,
                &snapshot,
                now,
                &scope.project_id,
                Scope::RunbooksRead,
            )
            .map_err(|_| (403, "auth.forbidden"))?;
            let configured = notify
                .slack_url(&scope)
                .await
                .map_err(|_| UNAVAILABLE)?
                .is_some();
            Ok((200, json!(ProjectSlackView { configured })))
        }
        "PUT" => {
            authorize(
                &record,
                &snapshot,
                now,
                &scope.project_id,
                Scope::ProjectAdmin,
            )
            .map_err(|_| (403, "auth.forbidden"))?;
            let request: ProjectSlackRequest =
                serde_json::from_value(r.body.clone()).map_err(|_| (422, "slack.invalid_url"))?;
            let url = request.url.trim().to_owned();
            if !url.starts_with("https://hooks.slack.com/") || url.len() > 512 {
                return Err((422, "slack.invalid_url"));
            }
            notify
                .set_slack_url(&scope, Some(url))
                .await
                .map_err(|_| UNAVAILABLE)?;
            Ok((200, json!(ProjectSlackView { configured: true })))
        }
        "DELETE" => {
            authorize(
                &record,
                &snapshot,
                now,
                &scope.project_id,
                Scope::ProjectAdmin,
            )
            .map_err(|_| (403, "auth.forbidden"))?;
            notify
                .set_slack_url(&scope, None)
                .await
                .map_err(|_| UNAVAILABLE)?;
            Ok((204, serde_json::Value::Null))
        }
        _ => Err((405, "request.method_not_allowed")),
    }
}
