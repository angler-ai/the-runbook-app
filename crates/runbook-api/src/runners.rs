//! The runner's core routes (runner design §12): the signing keys' JWKS,
//! and the runner registry. People manage their org's runner under
//! `/v1/orgs/{org}/runner` with `runner:read` / `runner:admin`; the runner
//! itself calls `/v1/runner-installs/*` with its own tokens, which only
//! [`crate::runner_access`] answers.
use crate::{Request, backends::Backends, problem::Response};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_core::{
    credentials::{AuthError, Scope},
    directory::{DirectoryStore, OrgRole},
    directory_operations::DirectoryOperations,
    runners::{
        NewInstall, NewRunner, RunnerError, RunnerPrincipal, RunnerSecretKind, RunnerStore,
        authorize_person, encode_runner_secret, token_hash, validate_url,
    },
};
use runbook_model::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// R0-1: the published Ed25519 keys.
pub const JWKS_PATH: &str = "/.well-known/jwks.json";

pub type Error = (u16, &'static str);

pub fn recognizes(path: &str) -> bool {
    path == JWKS_PATH
        || path.starts_with("/v1/runner-installs/")
        || path.starts_with("/v1/runner/decisions/")
        || org_runner_path(path)
        || crate::grants::recognizes(path)
        || crate::runner_proxy::recognizes(path)
}
fn org_runner_path(path: &str) -> bool {
    let mut parts = path.split('/');
    matches!(
        (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next()
        ),
        (Some(""), Some("v1"), Some("orgs"), Some(_), Some("runner"))
    )
}

pub(crate) fn storage<E>(_: E) -> Error {
    (503, "storage.unavailable")
}
pub(crate) fn runner_error(e: RunnerError) -> Error {
    e.status()
}
pub(crate) fn decode<T: DeserializeOwned>(r: &Request) -> Result<T, Error> {
    serde_json::from_value(r.body.clone()).map_err(|_| (422, "request.invalid"))
}
pub(crate) fn secret<B: Backends>(backends: &B, kind: RunnerSecretKind) -> Result<String, Error> {
    let mut bytes = [0u8; 32];
    backends
        .fill_random(&mut bytes)
        .map_err(|_| (503, "server.entropy"))?;
    Ok(encode_runner_secret(kind, bytes))
}
pub(crate) fn id<B: Backends, T: std::str::FromStr>(
    backends: &B,
    prefix: &str,
) -> Result<T, Error> {
    crate::backends::new_id(backends, prefix, backends.wall_clock_ms())
        .map_err(|_| (503, "server.entropy"))?
        .parse()
        .map_err(|_| (503, "server.entropy"))
}
fn ok(status: u16, body: Value, r: &Request) -> Response {
    Response::json(status, body, &r.request_id)
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    if r.path == JWKS_PATH {
        if r.method != "GET" {
            return Response::error(405, "request.method_not_allowed", &r.request_id);
        }
        return match backends.jwks().await {
            Ok(jwks) => ok(200, serde_json::to_value(jwks).unwrap_or_default(), &r),
            Err(e) => Response::error(503, e.code(), &r.request_id),
        };
    }
    if crate::runner_proxy::recognizes(&r.path) {
        return crate::runner_proxy::handle(backends, r).await;
    }
    let result = if org_runner_path(&r.path) {
        person(backends, &r).await
    } else if r.credential.is_none() {
        Err((401, "auth.required"))
    } else {
        // `/v1/runner-installs/*` and the grant routes take runner tokens
        // only; a person's credential never reaches them.
        Err((403, "auth.forbidden"))
    };
    match result {
        Ok((status, body)) => ok(status, body, &r),
        Err((status, code)) => Response::error(status, code, &r.request_id),
    }
}

fn vault_identity_key(value: String) -> Result<String, Error> {
    if value.len() != 43
        || !URL_SAFE_NO_PAD
            .decode(&value)
            .is_ok_and(|raw| raw.len() == 32)
    {
        return Err((422, "runner.vault_identity_invalid"));
    }
    Ok(value)
}

/// `/v1/orgs/{org}/runner…`: a person's credential.
async fn person<B: Backends>(backends: &B, r: &Request) -> Result<(u16, Value), Error> {
    let routes: [(&str, &str, Scope); 4] = [
        ("GET", "/v1/orgs/{org}/runner", Scope::RunnerRead),
        (
            "POST",
            "/v1/orgs/{org}/runner/bootstrap-tokens",
            Scope::RunnerAdmin,
        ),
        (
            "PUT",
            "/v1/orgs/{org}/runner/groups/{g}/projects",
            Scope::RunnerAdmin,
        ),
        (
            "PUT",
            "/v1/orgs/{org}/runner/vault-identity",
            Scope::RunnerAdmin,
        ),
    ];
    let mut recognized = false;
    let mut matched = None;
    for (method, path, scope) in routes {
        if let Some(params) = crate::directory::match_path(path, &r.path) {
            recognized = true;
            if method == r.method {
                matched = Some((path, params, scope));
            }
        }
    }
    let (path, params, scope) = matched.ok_or(if recognized {
        (405, "request.method_not_allowed")
    } else {
        (404, "resource.not_found")
    })?;
    let caller = r
        .credential_record(&backends.credentials())
        .await
        .map_err(|e| e.status())?;
    let org: OrganizationId = params["org"]
        .parse()
        .map_err(|_| (404, "resource.not_found"))?;
    if caller.org_id != org {
        return Err((404, "resource.not_found"));
    }
    if !caller.scopes.contains(&scope) {
        return Err((403, "auth.scope_missing"));
    }
    let directory = backends.directory();
    let now = directory.current_time_ms().await.map_err(storage)?;
    let snapshot = directory.snapshot(&org).await.map_err(storage)?;
    let actor = authorize_person(&caller, &snapshot, now, scope).map_err(|e| match e {
        AuthError::Forbidden => (403, "auth.forbidden"),
        _ => (401, "auth.invalid_credential"),
    })?;
    let store = backends.runners();
    let wall = backends.wall_clock_ms();
    match path {
        "/v1/orgs/{org}/runner" => {
            let status = store.status(&org, wall).await.map_err(runner_error)?;
            Ok((200, serde_json::to_value(status).map_err(storage)?))
        }
        "/v1/orgs/{org}/runner/bootstrap-tokens" => {
            let request: RunnerBootstrapRequest = decode(r)?;
            validate_url(&request.url, backends.runner_private_networks())
                .map_err(|code| (422, code))?;
            if store
                .install_of(&org)
                .await
                .map_err(runner_error)?
                .is_some()
            {
                return Err((409, "runner.already_registered"));
            }
            let token = secret(backends, RunnerSecretKind::Bootstrap)?;
            store
                .create_bootstrap(&org, &request.url, &token_hash(&token), wall)
                .await
                .map_err(runner_error)?;
            let view = RunnerBootstrapView {
                token,
                expires_at: wall + runbook_core::runners::BOOTSTRAP_TTL_MS,
            };
            Ok((201, serde_json::to_value(view).map_err(storage)?))
        }
        "/v1/orgs/{org}/runner/vault-identity" => {
            if actor.org_role != OrgRole::Owner {
                return Err((403, "auth.forbidden"));
            }
            let request: RunnerVaultIdentityRequest = decode(r)?;
            let key = vault_identity_key(request.public_key)?;
            store
                .set_vault_identity(&org, &actor.user_id, &key, wall)
                .await
                .map_err(runner_error)?;
            Ok((204, Value::Null))
        }
        _ => {
            let group: RunnerGroupId = params["g"]
                .parse()
                .map_err(|_| (404, "resource.not_found"))?;
            let request: RunnerGroupProjectsRequest = decode(r)?;
            if request
                .project_ids
                .iter()
                .any(|p| !snapshot.projects.contains_key(p))
            {
                return Err((422, "runner.project_unknown"));
            }
            store
                .set_group_projects(&org, &group, &request.project_ids)
                .await
                .map_err(runner_error)?;
            Ok((204, Value::Null))
        }
    }
}

/// `POST /v1/runner-installs/register` (bootstrap token).
pub(crate) async fn register<B: Backends>(
    backends: &B,
    r: &Request,
    bootstrap: &str,
) -> Result<(u16, Value), Error> {
    let request: RunnerRegisterRequest = decode(r)?;
    if !RUNNER_CONTRACT_VERSIONS.contains(&request.contract_version) {
        return Err((422, "runner.contract_unsupported"));
    }
    validate_url(&request.url, backends.runner_private_networks()).map_err(|code| (422, code))?;
    let directory_token = secret(backends, RunnerSecretKind::Directory)?;
    let release_token = secret(backends, RunnerSecretKind::Release)?;
    let join_token = secret(backends, RunnerSecretKind::Join)?;
    let install = backends
        .runners()
        .register(
            &token_hash(bootstrap),
            NewInstall {
                id: id(backends, "ri_")?,
                url: request.url,
                version: request.version,
                contract_version: request.contract_version,
                vault_identity_key: request
                    .vault_identity_key
                    .map(vault_identity_key)
                    .transpose()?,
                directory: token_hash(&directory_token),
                release: token_hash(&release_token),
                join: token_hash(&join_token),
            },
            backends.wall_clock_ms(),
        )
        .await
        .map_err(runner_error)?;
    let view = RunnerRegisteredView {
        install_id: install.id,
        directory_token,
        release_token,
        join_token,
    };
    Ok((201, serde_json::to_value(view).map_err(storage)?))
}

/// `POST /v1/runner-installs/{ri}/runners` (join token).
pub(crate) async fn join<B: Backends>(
    backends: &B,
    r: &Request,
    install: &RunnerInstallId,
    join: &str,
) -> Result<(u16, Value), Error> {
    let request: RunnerJoinRequest = decode(r)?;
    if request.name.trim().is_empty() || request.name.len() > 128 || request.group_name.len() > 128
    {
        return Err((422, "request.invalid"));
    }
    let claim_token = secret(backends, RunnerSecretKind::Claim)?;
    let runner_id = backends
        .runners()
        .join(
            install,
            &token_hash(join),
            NewRunner {
                id: id(backends, "rn_")?,
                name: request.name,
                group_id: request.group_id,
                group_name: request.group_name,
                version: request.version,
                claim: token_hash(&claim_token),
            },
            backends.wall_clock_ms(),
        )
        .await
        .map_err(runner_error)?;
    let view = RunnerJoinedView {
        runner_id,
        claim_token,
    };
    Ok((201, serde_json::to_value(view).map_err(storage)?))
}

/// `PUT|DELETE /v1/runner-installs/{ri}/agents/{agent_id}` (directory
/// token): sync one agent's current version into the directory.
pub(crate) async fn agent<B: Backends>(
    backends: &B,
    r: &Request,
    principal: &RunnerPrincipal,
    agent_id: &str,
) -> Result<(u16, Value), Error> {
    use runbook_core::agent_directory::{AgentDirectory, validate_entry};
    let store = backends.runners();
    if r.method == "DELETE" {
        store
            .remove_agent(&principal.install_id, agent_id)
            .await
            .map_err(runner_error)?;
        return Ok((204, Value::Null));
    }
    let entry: AgentDirectoryEntry = decode(r)?;
    validate_entry(agent_id, &entry).map_err(runner_error)?;
    store
        .upsert_agent(&principal.install_id, &entry, backends.wall_clock_ms())
        .await
        .map_err(runner_error)?;
    Ok((204, Value::Null))
}

/// `POST /v1/runner-installs/{ri}/heartbeat` (directory token).
pub(crate) async fn heartbeat<B: Backends>(
    backends: &B,
    r: &Request,
    principal: &RunnerPrincipal,
) -> Result<(u16, Value), Error> {
    let request: RunnerHeartbeatRequest = decode(r)?;
    backends
        .runners()
        .heartbeat(&principal.install_id, &request, backends.wall_clock_ms())
        .await
        .map_err(runner_error)?;
    Ok((204, json!(null)))
}
