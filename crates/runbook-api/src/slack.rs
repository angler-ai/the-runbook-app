//! SLACK1 stage 1: a project's Slack incoming-webhook URL. The URL is
//! a credential: it is written with project-admin authority, never read
//! back (reads say only whether one is configured), and never logged.
use crate::{Request, backends::Backends, problem::Response};
use runbook_core::{
    credentials::{Scope, authorize},
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    notify::NotifyStore,
    slack::{SlackTarget, valid_channel},
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
            let stored = notify
                .slack_settings(&scope)
                .await
                .map_err(|_| UNAVAILABLE)?;
            Ok((
                200,
                json!(settings_view(stored, backends.slack_app().is_some())),
            ))
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
            let target = match (request.url, request.channel) {
                (Some(url), None) => {
                    let url = url.trim().to_owned();
                    if !url.starts_with("https://hooks.slack.com/") || url.len() > 512 {
                        return Err((422, "slack.invalid_url"));
                    }
                    Some(SlackTarget::Webhook(url))
                }
                (None, Some(channel)) => {
                    let channel = channel.trim().to_owned();
                    if !valid_channel(&channel) {
                        return Err((422, "slack.invalid_channel"));
                    }
                    // A channel needs the server's Slack app to post.
                    if backends.slack_app().is_none() {
                        return Err((409, "slack.app_not_configured"));
                    }
                    Some(SlackTarget::Channel(channel))
                }
                (None, None) if request.notify_run_started.is_some() => None,
                _ => return Err((422, "slack.invalid_url")),
            };
            let stored = notify
                .save_slack_settings(
                    &scope,
                    target.map(|target| target.stored()),
                    request.notify_run_started,
                )
                .await
                .map_err(|_| UNAVAILABLE)?
                .ok_or((409, "slack.target_required"))?;
            Ok((
                200,
                json!(settings_view(Some(stored), backends.slack_app().is_some())),
            ))
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
                .remove_slack_settings(&scope)
                .await
                .map_err(|_| UNAVAILABLE)?;
            Ok((204, serde_json::Value::Null))
        }
        _ => Err((405, "request.method_not_allowed")),
    }
}

fn settings_view(
    stored: Option<runbook_core::notify::ProjectSlackSettings>,
    app: bool,
) -> ProjectSlackView {
    let channel = match stored
        .as_ref()
        .map(|settings| SlackTarget::parse(&settings.target))
    {
        Some(SlackTarget::Channel(channel)) => Some(channel),
        _ => None,
    };
    ProjectSlackView {
        configured: stored.is_some(),
        channel,
        app,
        notify_run_started: stored.is_some_and(|settings| settings.notify_run_started),
    }
}
