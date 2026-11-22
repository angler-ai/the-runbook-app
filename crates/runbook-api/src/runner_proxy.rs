//! R0-5: the proxy from the web app (and people's agents) to the org's
//! runner (runner design §11.4). `ANY /v1/runners/{ri}/proxy/{*path}`
//! forwards to the runner's own API at `{runner url}/{path}` only when
//! `{path}` is one of the runner's public routes
//! ([`runner_routes.json`](./runner_routes.json)), the caller holds a
//! runner scope and sent `X-Runbook-CSRF: 1`. The core authenticates the
//! caller, never forwards cookies or its own credentials, and adds a
//! request-bound user assertion (`Runbook-Assertion`, `typ: rb-user+jwt`,
//! 60 seconds, single use, method, path and body hash bound). The runner
//! decides what the person may do. Responses must be JSON (an event stream
//! only on the transcript route) and are served with `nosniff` and
//! `Content-Security-Policy: sandbox`, without cookies.
use crate::{Request, backends::Backends, problem::Response};
use runbook_core::{
    credentials::{ActorKind, AuthError, CredentialPrincipal, Scope},
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    runners::RunnerStore,
    signing::{
        JwtSigner, KeyPurpose, USER_ASSERTION_TTL_S, USER_TYP, UserAssertionClaims, body_hash,
    },
};
use runbook_model::{RunnerInstallId, RunnerInstallStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::OnceLock};

/// The CSRF header every proxied request carries, whatever its credential.
pub const CSRF_HEADER: &str = "x-runbook-csrf";
/// The header carrying the user assertion to the runner.
pub const ASSERTION_HEADER: &str = "runbook-assertion";
/// The most a proxied response may hold.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// How long the core waits for the runner.
pub const TIMEOUT_MS: u64 = 30_000;

/// `/v1/runners/{ri}/proxy/{*path}`.
pub fn recognizes(path: &str) -> bool {
    parse(path).is_some()
}
pub(crate) fn proxied_path(path: &str) -> Option<String> {
    parse(path).map(|(_, target)| format!("/{target}"))
}
fn parse(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/v1/runners/")?;
    let (install, rest) = rest.split_once('/')?;
    let target = rest.strip_prefix("proxy/")?;
    (!install.is_empty() && !target.is_empty()).then_some((install, target))
}

/// One of the runner's public routes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerRoute {
    pub method: String,
    pub path: String,
    /// The runner scope a caller must hold for this route (`runner:read`
    /// or `runner:admin`); scopes never imply each other.
    pub scope: Scope,
    pub sensitive: bool,
    pub mfa_reason: String,
    /// The route may answer with an event stream.
    #[serde(default, rename = "sse", skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerRoutes {
    /// The runner's route-list format version.
    pub version: u32,
    pub prefix: String,
    pub routes: Vec<RunnerRoute>,
}
/// The checked-in allow-list (the runner's public route list).
pub const ROUTES_JSON: &str = include_str!("runner_routes.json");
pub fn routes() -> &'static RunnerRoutes {
    static ROUTES: OnceLock<RunnerRoutes> = OnceLock::new();
    ROUTES.get_or_init(|| serde_json::from_str(ROUTES_JSON).expect("runner_routes.json"))
}
/// The allowed route a request names, if any. Every path parameter must be
/// one plain segment as sent: `[A-Za-z0-9_.:-]+`, not `.` or `..`, and
/// never percent-encoded (so no encoded `/`, `\` or dots reach the runner).
pub fn route_for(method: &str, path: &str) -> Option<&'static RunnerRoute> {
    if path.contains("/../") || path.contains("//") || path.ends_with("/..") {
        return None;
    }
    routes()
        .routes
        .iter()
        .find(|r| r.method == method && plain_match(&r.path, path))
}
fn plain_match(template: &str, path: &str) -> bool {
    let a: Vec<_> = template.split('/').collect();
    let b: Vec<_> = path.split('/').collect();
    a.len() == b.len()
        && a.iter().zip(&b).all(|(a, b)| {
            if a.starts_with('{') && a.ends_with('}') {
                plain_segment(b)
            } else {
                a == b
            }
        })
}
fn plain_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// What the host's request decoding keeps for the proxy: whether the CSRF
/// header was `1`, the raw query, and the JSON body.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProxyEnvelope {
    pub csrf: bool,
    pub query: Option<String>,
    pub body: Value,
}

/// One request to the runner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerHttpRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
/// The runner's answer (the host reads at most [`MAX_RESPONSE_BYTES`] and
/// waits at most [`TIMEOUT_MS`]; it never follows redirects).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerHttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

fn error(status: u16, code: &str, r: &Request) -> Response {
    Response::error(status, code, &r.request_id)
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match proxy(backends, &r).await {
        Ok(response) => response,
        Err((status, code)) => error(status, code, &r),
    }
}

type Error = (u16, &'static str);

async fn proxy<B: Backends>(backends: &B, r: &Request) -> Result<Response, Error> {
    let (install, target) = parse(&r.path).ok_or((404, "resource.not_found"))?;
    let install: RunnerInstallId = install.parse().map_err(|_| (404, "resource.not_found"))?;
    let runner_path = format!("/{target}");
    let envelope: ProxyEnvelope = serde_json::from_value(r.body.clone()).unwrap_or_default();
    let caller = r
        .credential_record(&backends.credentials())
        .await
        .map_err(|e| e.status())?;
    if !envelope.csrf {
        return Err((403, "auth.csrf"));
    }
    let route = route_for(&r.method, &runner_path).ok_or((404, "runner.route_unknown"))?;
    // The route's own scope is required here (scopes never imply each
    // other); the runner still decides per action and per project.
    if !Scope::RUNNER.contains(&route.scope) || !caller.scopes.contains(&route.scope) {
        return Err((403, "auth.scope_missing"));
    }
    let directory = backends.directory();
    let now = directory
        .current_time_ms()
        .await
        .map_err(|_| (503, "storage.unavailable"))?;
    let snapshot = directory
        .snapshot(&caller.org_id)
        .await
        .map_err(|_| (503, "storage.unavailable"))?;
    let actor =
        runbook_core::runners::authorize_proxy(&caller, &snapshot, now).map_err(|e| match e {
            AuthError::Forbidden => (403, "auth.forbidden"),
            _ => (401, "auth.invalid_credential"),
        })?;
    let store = backends.runners();
    let record = store
        .install_of(&caller.org_id)
        .await
        .map_err(|e| e.status())?
        .filter(|i| i.id == install)
        .ok_or((404, "resource.not_found"))?;
    let status = store
        .status(&caller.org_id, backends.wall_clock_ms())
        .await
        .map_err(|e| e.status())?;
    if status.status != RunnerInstallStatus::Online {
        return Err((503, "runner.offline"));
    }
    let body = if envelope.body.is_null() {
        vec![]
    } else {
        serde_json::to_vec(&envelope.body).map_err(|_| (422, "request.invalid"))?
    };
    let htu = match envelope.query.as_deref().filter(|q| !q.is_empty()) {
        Some(query) => format!("{runner_path}?{query}"),
        None => runner_path.clone(),
    };
    let mut jti = [0u8; 16];
    backends
        .fill_random(&mut jti)
        .map_err(|_| (503, "server.entropy"))?;
    let at = backends.wall_clock_ms() / 1000;
    // A project-restricted credential asserts only its projects and the
    // org role `member`.
    let (org_role, roles) = runbook_core::runners::asserted_roles(&caller, &snapshot, &actor);
    let project_roles: BTreeMap<String, String> = roles
        .into_iter()
        .filter_map(|(project, role)| {
            let name = serde_json::to_value(role).ok()?.as_str()?.to_owned();
            Some((project.to_string(), name))
        })
        .collect();
    let org_role = serde_json::to_value(org_role)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "member".into());
    let (principal, sponsor) = match (&caller.principal, actor.actor) {
        (CredentialPrincipal::User { user_id, .. }, ActorKind::Agent) => {
            ("agent", Some(user_id.to_string()))
        }
        _ => ("human", None),
    };
    let claims = UserAssertionClaims {
        iss: backends.origin(),
        aud: format!("runner:{install}"),
        jti: jti.iter().map(|b| format!("{b:02x}")).collect(),
        iat: at,
        exp: at + USER_ASSERTION_TTL_S,
        org_id: caller.org_id.to_string(),
        user_id: actor.user_id.to_string(),
        principal: principal.into(),
        sponsor_id: sponsor,
        org_role,
        project_roles,
        scopes: actor
            .scopes
            .iter()
            .filter_map(|s| serde_json::to_value(s).ok()?.as_str().map(str::to_owned))
            .collect(),
        htm: r.method.clone(),
        htu: htu.clone(),
        bh: body_hash(&body),
    };
    let keys = backends.signing_keys().await.map_err(|e| (503, e.code()))?;
    let assertion = keys
        .sign(KeyPurpose::UserAssertion, USER_TYP, &claims)
        .map_err(|_| (503, "signing.unavailable"))?;
    let mut headers = vec![
        ("accept".to_owned(), "application/json".to_owned()),
        (ASSERTION_HEADER.to_owned(), assertion),
        ("x-request-id".to_owned(), r.request_id.clone()),
    ];
    if !body.is_empty() {
        headers.push(("content-type".into(), "application/json".into()));
    }
    let answer = backends
        .runner_fetch(RunnerHttpRequest {
            url: format!("{}{htu}", record.url.trim_end_matches('/')),
            method: r.method.clone(),
            headers,
            body,
        })
        .await
        .map_err(|code| match code {
            "runner.timeout" => (504, "runner.timeout"),
            "runner.bad_response" => (502, "runner.bad_response"),
            "runner.unsupported" => (501, "runner.unsupported"),
            "runner.url_forbidden" => (502, "runner.url_forbidden"),
            _ => (502, "runner.unreachable"),
        })?;
    runner_response(answer, route, &r.request_id)
}

fn runner_response(
    answer: RunnerHttpResponse,
    route: &RunnerRoute,
    request_id: &str,
) -> Result<Response, (u16, &'static str)> {
    if answer.body.len() > MAX_RESPONSE_BYTES {
        return Err((502, "runner.bad_response"));
    }
    let media = answer
        .content_type
        .as_deref()
        .and_then(|t| t.split(';').next())
        .map(|t| t.trim().to_ascii_lowercase());
    let content_type = match media.as_deref() {
        Some("application/json") => "application/json",
        Some("application/problem+json") => "application/problem+json",
        Some("text/event-stream") if route.stream => "text/event-stream",
        None if answer.body.is_empty() => "application/json",
        _ => return Err((502, "runner.bad_response")),
    };
    // Browser MFA prompts belong to the core, including its gate on runner routes.
    if answer.status == 403
        && content_type != "text/event-stream"
        && let Ok(body) = serde_json::from_slice::<Value>(&answer.body)
        && matches!(
            body["code"].as_str(),
            Some("auth.mfa_step_up" | "auth.mfa_required")
        )
    {
        return Ok(Response::error(403, "runner.forbidden", request_id));
    }
    Ok(Response::proxied(
        answer.status,
        content_type,
        answer.body,
        request_id,
    ))
}

/// The proxy body as the host's request decoding builds it.
pub fn envelope(csrf: Option<&str>, query: Option<&str>, body: Value) -> Value {
    serde_json::to_value(ProxyEnvelope {
        csrf: csrf.is_some_and(|v| v.trim() == "1"),
        query: query.map(str::to_owned),
        body,
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_refusals_cannot_request_core_mfa() {
        let route = route_for("POST", "/runner/v1/groups").expect("runner group route");
        for code in ["auth.mfa_step_up", "auth.mfa_required"] {
            let response = runner_response(
                RunnerHttpResponse {
                    status: 403,
                    content_type: Some("application/problem+json".into()),
                    body: serde_json::to_vec(&serde_json::json!({"status": 403, "code": code}))
                        .unwrap(),
                },
                route,
                "request",
            )
            .unwrap();
            assert_eq!(response.status, 403);
            assert_eq!(response.body["code"], "runner.forbidden");
        }
    }
}
