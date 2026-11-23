//! R0-2: runner tokens reach an exact route allow-list per purpose (the
//! pattern of `execution_access.rs`). A runner token never reaches a
//! person-facing handler, and a person's credential never reaches these.
use crate::{
    Request,
    backends::Backends,
    problem::Response,
    runners::{Error, runner_error},
};
use runbook_core::runners::{
    RunnerPrincipal, RunnerPurpose, RunnerSecretKind, RunnerStore, runner_secret_kind, token_hash,
};
use runbook_model::RunnerInstallId;
use serde_json::Value;
use std::collections::BTreeMap;

/// Who may call what: (purpose, method, path). `{ri}` must be the token's
/// own install.
pub const ALLOWED: &[(RunnerPurpose, &str, &str)] = &[
    (
        RunnerPurpose::Join,
        "POST",
        "/v1/runner-installs/{ri}/runners",
    ),
    (
        RunnerPurpose::Directory,
        "POST",
        "/v1/runner-installs/{ri}/heartbeat",
    ),
    (
        RunnerPurpose::Directory,
        "PUT",
        "/v1/runner-installs/{ri}/agents/{agent_id}",
    ),
    (
        RunnerPurpose::Directory,
        "DELETE",
        "/v1/runner-installs/{ri}/agents/{agent_id}",
    ),
    (
        RunnerPurpose::Directory,
        "POST",
        "/v1/runner-installs/{ri}/waiting",
    ),
    (
        RunnerPurpose::Directory,
        "POST",
        "/v1/runner-installs/{ri}/events",
    ),
    (
        RunnerPurpose::Directory,
        "GET",
        "/v1/runner-installs/{ri}/groups",
    ),
    (
        RunnerPurpose::Directory,
        "GET",
        "/v1/runner-installs/{ri}/ready",
    ),
    (
        RunnerPurpose::Claim,
        "GET",
        "/v1/step-runs/{sr}/context/run",
    ),
    (
        RunnerPurpose::Claim,
        "GET",
        "/v1/step-runs/{sr}/context/step",
    ),
    (
        RunnerPurpose::Claim,
        "GET",
        "/v1/step-runs/{sr}/context/runs",
    ),
    (
        RunnerPurpose::Claim,
        "GET",
        "/v1/step-runs/{sr}/context/past-run",
    ),
    (
        RunnerPurpose::Claim,
        "GET",
        "/v1/step-runs/{sr}/context/memory",
    ),
    (
        RunnerPurpose::Claim,
        "POST",
        "/v1/step-runs/{sr}/context/memory",
    ),
    (
        RunnerPurpose::Directory,
        "GET",
        "/v1/runner-installs/{ri}/projects/{project_id}/skills/{name}",
    ),
    (
        RunnerPurpose::Claim,
        "POST",
        "/v1/runner/decisions/{lease}/decide",
    ),
    (
        RunnerPurpose::Claim,
        "POST",
        "/v1/runner/decisions/{lease}/abstain",
    ),
    (RunnerPurpose::Claim, "POST", "/v1/step-runs/{sr}/claim"),
    (RunnerPurpose::Claim, "POST", "/v1/step-runs/{sr}/heartbeat"),
    (RunnerPurpose::Claim, "POST", "/v1/step-runs/{sr}/logs"),
    (RunnerPurpose::Claim, "POST", "/v1/step-runs/{sr}/checklist"),
    (RunnerPurpose::Claim, "POST", "/v1/step-runs/{sr}/report"),
    (RunnerPurpose::Claim, "POST", "/v1/step-runs/{sr}/release"),
    (RunnerPurpose::Claim, "GET", "/v1/step-runs/{sr}"),
    (
        RunnerPurpose::Claim,
        "GET",
        "/v1/step-runs/{sr}/skill-files",
    ),
    (RunnerPurpose::Release, "POST", "/v1/step-runs/{sr}/grants"),
    (
        RunnerPurpose::Release,
        "POST",
        "/v1/step-runs/{sr}/grants/revoked",
    ),
];

/// A runner secret (any runner prefix) as the bearer.
pub fn is_runner(r: &Request) -> bool {
    r.bearer().is_some_and(|s| runner_secret_kind(s).is_some())
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok((status, body)) => Response::json(status, body, &r.request_id),
        Err((status, code)) => Response::error(status, code, &r.request_id),
    }
}

fn forbidden() -> Error {
    (403, "auth.forbidden")
}

/// The route a runner token may call, with its path parameters.
pub fn allowed(
    purpose: RunnerPurpose,
    method: &str,
    path: &str,
) -> Option<(&'static str, BTreeMap<String, String>)> {
    ALLOWED.iter().find_map(|(p, m, template)| {
        (*p == purpose && *m == method)
            .then(|| crate::directory::match_path(template, path))
            .flatten()
            .map(|params| (*template, params))
    })
}

async fn dispatch<B: Backends>(backends: &B, r: &Request) -> Result<(u16, Value), Error> {
    let secret = r.bearer().ok_or((401, "auth.required"))?;
    let kind = runner_secret_kind(secret).ok_or((401, "auth.invalid_credential"))?;
    if kind == RunnerSecretKind::Bootstrap {
        // The bootstrap token only registers; the store checks it is
        // unused, unexpired and for this URL.
        if r.method != "POST" || r.path != "/v1/runner-installs/register" {
            return Err(forbidden());
        }
        return crate::runners::register(backends, r, secret).await;
    }
    let principal = backends
        .runners()
        .authenticate(&token_hash(secret))
        .await
        .map_err(runner_error)?
        .ok_or((401, "auth.invalid_credential"))?;
    let (template, params) =
        allowed(principal.purpose, &r.method, &r.path).ok_or_else(forbidden)?;
    if let Some(ri) = params.get("ri") {
        let ri: RunnerInstallId = ri.parse().map_err(|_| forbidden())?;
        if ri != principal.install_id {
            return Err(forbidden());
        }
    }
    backends
        .trace_authenticated_runner(route(backends, r, &principal, template, &params, secret))
        .await
}

async fn route<B: Backends>(
    backends: &B,
    r: &Request,
    principal: &RunnerPrincipal,
    template: &str,
    params: &BTreeMap<String, String>,
    secret: &str,
) -> Result<(u16, Value), Error> {
    use crate::grants;
    match template {
        "/v1/runner/decisions/{lease}/decide" | "/v1/runner/decisions/{lease}/abstain" => {
            grants::finish_decision(
                backends,
                r,
                principal,
                &params["lease"],
                template.ends_with("/decide"),
            )
            .await
        }
        "/v1/runner-installs/{ri}/projects/{project_id}/skills/{name}" => {
            crate::runner_context::skill(
                backends,
                principal,
                &params["project_id"],
                &params["name"],
            )
            .await
        }

        template if template.starts_with("/v1/step-runs/{sr}/context/") => {
            crate::runner_context::handle(backends, r, principal, &params["sr"]).await
        }
        "/v1/runner-installs/{ri}/waiting" => grants::waiting(backends, r, principal).await,
        "/v1/runner-installs/{ri}/events" => grants::events(backends, r, principal).await,
        "/v1/runner-installs/{ri}/groups" => {
            let registry = backends
                .runners()
                .status(&principal.org_id, backends.wall_clock_ms())
                .await
                .map_err(runner_error)?;
            if registry.install_id.as_ref() != Some(&principal.install_id) {
                return Err(forbidden());
            }
            Ok((
                200,
                serde_json::json!(runbook_model::RunnerGroupRegistryView {
                    install_id: principal.install_id.clone(),
                    groups: registry.groups,
                }),
            ))
        }
        "/v1/runner-installs/{ri}/ready" => grants::ready(backends, r, principal).await,
        "/v1/step-runs/{sr}/grants" => grants::issue(backends, r, principal, params).await,
        "/v1/step-runs/{sr}/grants/revoked" => {
            grants::revoked(backends, r, principal, params).await
        }
        "/v1/step-runs/{sr}" => {
            grants::step_operation(backends, r, principal, params, "step").await
        }
        "/v1/step-runs/{sr}/skill-files" => {
            grants::step_operation(backends, r, principal, params, "skill").await
        }
        "/v1/step-runs/{sr}/claim" => {
            grants::step_operation(backends, r, principal, params, "claim").await
        }
        "/v1/step-runs/{sr}/heartbeat" => {
            grants::step_operation(backends, r, principal, params, "heartbeat").await
        }
        "/v1/step-runs/{sr}/logs" => {
            grants::step_operation(backends, r, principal, params, "logs").await
        }
        "/v1/step-runs/{sr}/checklist" => {
            grants::step_operation(backends, r, principal, params, "checklist").await
        }
        "/v1/step-runs/{sr}/report" => {
            grants::step_operation(backends, r, principal, params, "report").await
        }
        "/v1/step-runs/{sr}/release" => {
            grants::step_operation(backends, r, principal, params, "release").await
        }
        "/v1/runner-installs/{ri}/agents/{agent_id}" => {
            crate::runners::agent(backends, r, principal, &params["agent_id"]).await
        }
        "/v1/runner-installs/{ri}/runners" => {
            crate::runners::join(backends, r, &principal.install_id, secret).await
        }
        "/v1/runner-installs/{ri}/heartbeat" => {
            crate::runners::heartbeat(backends, r, principal).await
        }
        _ => Err((404, "resource.not_found")),
    }
}
