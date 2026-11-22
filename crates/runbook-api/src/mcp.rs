//! MCP1: the remote MCP endpoint (PRD §11), portable (CF0). MCP Streamable
//! HTTP, stateless: one JSON-RPC message per `POST /mcp`, answered with one
//! JSON body. There is no SSE stream and no `Mcp-Session-Id`; `GET` (and
//! every other method) is 405, as the transport allows for a server without
//! a stream.
//!
//! The tools, resources and prompt are `runbook-mcp`'s, unchanged: a
//! [`RouterBackend`] answers each `HttpCall` with the API [`Router`] in
//! process, as an ordinary request carrying the caller's bearer, so every
//! call is authorized exactly as the same token is on the REST API. Cookies
//! are never read here (D46).
//!
//! A host runs the steps in order: [`precheck`] (headers only), its own
//! concurrency limit, [`authenticate`], its body limits and [`parse`], then
//! [`answer`]. [`handle`] chains them for a host that has the whole body
//! and no limits of its own.
use crate::{
    Credential, Request,
    actors::ActorBackend,
    backends::{Backends, new_id},
    http::{Headers, HttpResponse, bearer, is_json},
    problem::Response,
    router::Router,
};
use runbook_core::{
    catalog::CatalogStore,
    credentials::{CredentialRecord, Scope, TokenKind, authorize, parse_secret, secret_hash},
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    execution_credentials::ExecutionCredentialStore,
};
use runbook_mcp::backend::{AsyncBackend, BackendError, HttpCall, HttpReply};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

pub const PATH: &str = "/mcp";

// ------------------------------------------------------------------ HTTP

fn builder(status: u16, id: &str) -> HttpResponse {
    HttpResponse::new(status)
        .header("x-request-id", id)
        .header("x-ratelimit-limit", "unlimited")
        .header("x-ratelimit-remaining", "unlimited")
        .header("cache-control", "no-store")
}

fn json_response(status: u16, id: &str, body: &Value) -> HttpResponse {
    let mut response = builder(status, id).header("content-type", "application/json");
    response.body = serde_json::to_vec(body).expect("JSON response");
    response
}

/// 202: a notification, or a response to a server request; nothing to say.
pub fn accepted(id: &str) -> HttpResponse {
    builder(202, id)
}

/// A transport-level refusal: a JSON-RPC error with no request ID, whose
/// `data.code` is the API problem code.
pub fn refusal(status: u16, id: &str, rpc: i64, code: &str, message: &str) -> HttpResponse {
    let mut body = runbook_mcp::error(Value::Null, rpc, message);
    body["error"]["data"] = json!({"code": code});
    json_response(status, id, &body)
}

fn unauthorized(id: &str, code: &str, challenge: &str) -> HttpResponse {
    refusal(
        401,
        id,
        -32000,
        code,
        "Unauthorized: send Authorization: Bearer <token>",
    )
    .header("www-authenticate", challenge)
}

/// The message did not fit the body limit (413).
pub fn too_large(id: &str) -> HttpResponse {
    refusal(
        413,
        id,
        -32000,
        "request.body_too_large",
        "Request body too large",
    )
}

/// `Accept` must allow a JSON response; no `Accept` header allows anything.
fn accepts_json(headers: &Headers) -> bool {
    let values: Vec<&str> = headers.get_all_str("accept").collect();
    if values.is_empty() {
        return true;
    }
    values.iter().flat_map(|v| v.split(',')).any(|range| {
        let mut parts = range.split(';');
        let media = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let refused = parts.any(|p| {
            p.trim()
                .strip_prefix("q=")
                .and_then(|q| q.trim().parse::<f32>().ok())
                .is_some_and(|q| q <= 0.0)
        });
        !refused && matches!(media.as_str(), "application/json" | "application/*" | "*/*")
    })
}

/// The header checks, before any work: origin (DNS rebinding), method,
/// bearer, `Accept`, `MCP-Protocol-Version` and `Content-Type`. Returns the
/// bearer secret.
pub fn precheck(
    method: &str,
    headers: &Headers,
    origin: &str,
    id: &str,
) -> Result<String, HttpResponse> {
    // DNS rebinding: a browser's request always carries its page's origin.
    if headers.contains("origin")
        && !headers
            .get("origin")
            .is_some_and(|o| o.eq_ignore_ascii_case(origin))
    {
        return Err(refusal(
            403,
            id,
            -32000,
            "mcp.origin_forbidden",
            "Forbidden origin",
        ));
    }
    if method != "POST" {
        return Err(refusal(
            405,
            id,
            -32000,
            "request.method_not_allowed",
            "Method not allowed: this server has no event stream and no sessions; POST one JSON-RPC message",
        )
        .header("allow", "POST"));
    }
    // D46: bearer only; a cookie is never read on this path.
    let Some(secret) = bearer(headers) else {
        return Err(unauthorized(id, "auth.required", "Bearer"));
    };
    if !accepts_json(headers) {
        return Err(refusal(
            406,
            id,
            -32000,
            "request.not_acceptable",
            "Not acceptable: this server answers with application/json",
        ));
    }
    if headers.contains("mcp-protocol-version")
        && !headers
            .get("mcp-protocol-version")
            .is_some_and(|v| runbook_mcp::HTTP_PROTOCOLS.contains(&v))
    {
        return Err(refusal(
            400,
            id,
            -32000,
            "mcp.protocol_version",
            "Unsupported MCP-Protocol-Version: this endpoint serves 2025-06-18",
        ));
    }
    if !is_json(headers) {
        return Err(refusal(
            415,
            id,
            -32000,
            "request.content_type",
            "Send Content-Type: application/json",
        ));
    }
    Ok(secret)
}

/// The declared `Content-Length`, if any (400 when it is not a number).
pub fn declared_length(headers: &Headers, id: &str) -> Result<Option<u64>, HttpResponse> {
    match headers.get_all("content-length").next() {
        None => Ok(None),
        Some(_) => match headers.get("content-length").and_then(|v| v.parse().ok()) {
            Some(n) => Ok(Some(n)),
            None => Err(refusal(
                400,
                id,
                -32000,
                "request.invalid_length",
                "Invalid Content-Length",
            )),
        },
    }
}

/// One JSON-RPC message from the body. `Err` is the answer: a refusal, or
/// 202 for a response to a server request (this server sends none).
pub fn parse(bytes: &[u8], id: &str) -> Result<Value, HttpResponse> {
    let message: Value = serde_json::from_slice(bytes)
        .map_err(|_| refusal(400, id, -32700, "request.invalid_json", "Parse error"))?;
    let Some(object) = message.as_object() else {
        let text = if message.is_array() {
            "Batches are not supported; send one JSON-RPC message per request"
        } else {
            "Invalid Request"
        };
        return Err(refusal(400, id, -32600, "mcp.invalid_request", text));
    };
    if !object.contains_key("method") {
        if object.contains_key("id")
            && (object.contains_key("result") || object.contains_key("error"))
        {
            return Err(accepted(id));
        }
        return Err(refusal(
            400,
            id,
            -32600,
            "mcp.invalid_request",
            "Invalid Request",
        ));
    }
    Ok(message)
}

/// What the bearer is.
#[derive(Clone, Default)]
pub struct Authenticated {
    /// The run an execution credential is bound to.
    pub execution: Option<String>,
    /// An execution credential revoked because its run ended (the run is
    /// terminal): the tools answer `exit` for that run.
    pub ended: bool,
    /// MCP2: an OAuth access token's introspection, linked; the tools call
    /// the API with it as a session credential instead of the bearer.
    pub oauth: Option<crate::ResolvedSession>,
}

/// MCP2: an OAuth access token, introspected by the host (`looked_up`) and
/// linked once here (D45). Unknown, expired, revoked, another resource's or
/// not a grant: 401 with an `invalid_token` challenge; the person's
/// directory refusal: 403.
pub async fn authenticate_oauth<B: Backends>(
    backends: &B,
    looked_up: Result<Option<crate::ResolvedSession>, crate::session::Unavailable>,
    id: &str,
) -> Result<Authenticated, HttpResponse> {
    let found = match looked_up {
        Ok(Some(mut token)) if token.scopes.is_some() && token.email_verified => {
            crate::session::link(backends, &mut token)
                .await
                .map(|()| token)
        }
        Ok(_) => Err((401, "")),
        Err(_) => Err((503, "")),
    };
    match found {
        Ok(token) => Ok(Authenticated {
            oauth: Some(token),
            ..Authenticated::default()
        }),
        Err((401, _)) => Err(unauthorized(
            id,
            "auth.invalid_credential",
            "Bearer error=\"invalid_token\"",
        )),
        Err((403, code)) => Err(refusal(403, id, -32000, code, "Forbidden")),
        Err(_) => Err(refusal(
            503,
            id,
            -32603,
            "auth.unavailable",
            "Unavailable; retry",
        )),
    }
}

/// MCP2: every 401 on `/mcp` names the protected resource metadata when an
/// authorization server is configured (RFC 9728 §5.1), keeping
/// `error="invalid_token"` for a presented token that failed.
pub fn with_challenge(mut response: HttpResponse, origin: &str, oauth: bool) -> HttpResponse {
    if response.status == 401 {
        let invalid = response
            .header_value("www-authenticate")
            .is_some_and(|v| v.contains("invalid_token"));
        response.set_header(
            "www-authenticate",
            crate::oauth::challenge(origin, oauth, invalid),
        );
    }
    response
}

fn nonce<B: Backends>(backends: &B) -> Option<String> {
    new_id(backends, "", backends.wall_clock_ms()).ok()
}

fn bearer_request(path: &str, secret: &str, id: &str, nonce: String) -> Request {
    Request {
        method: "POST".into(),
        path: path.into(),
        query: BTreeMap::new(),
        body: Value::Null,
        credential: Some(Credential::Bearer(secret.to_owned())),
        idempotency_key: None,
        request_id: id.into(),
        new_id: nonce,
        if_match: None,
        client_ip: None,
    }
}

/// Which credential the bearer is, or the answer: 401 with an
/// `invalid_token` challenge, or 503.
pub async fn authenticate<B: Backends>(
    backends: &B,
    secret: &str,
    id: &str,
) -> Result<Authenticated, HttpResponse> {
    let Some(nonce) = nonce(backends) else {
        return Err(refusal(503, id, -32603, "server.entropy", "Unavailable"));
    };
    let request = bearer_request("/mcp", secret, id, nonce);
    match credential_kind(backends, &request).await {
        Ok(body) => Ok(Authenticated {
            execution: body["execution_id"].as_str().map(str::to_owned),
            ended: body["ended"] == json!(true),
            oauth: None,
        }),
        Err((401, _)) => Err(unauthorized(
            id,
            "auth.invalid_credential",
            "Bearer error=\"invalid_token\"",
        )),
        Err(_) => Err(refusal(
            503,
            id,
            -32603,
            "server.unavailable",
            "Unavailable; retry",
        )),
    }
}

/// The host's hook around each in-process API call of a message (CF0
/// review). Natively it is a worker slot, held for one call at a time, so
/// a long message never holds one between its calls and REST interleaves.
pub trait CallSlots {
    type Slot;
    /// A slot for one call, or the problem code that fails the call (for
    /// example `server.timeout` once the message's deadline passed).
    fn acquire(&self) -> impl std::future::Future<Output = Result<Self::Slot, &'static str>>;
}

/// No per-call limit (a host whose runtime bounds concurrency itself).
pub struct Unlimited;
impl CallSlots for Unlimited {
    type Slot = ();
    async fn acquire(&self) -> Result<(), &'static str> {
        Ok(())
    }
}

/// Run one JSON-RPC message with the caller's bearer. `max_calls` bounds
/// the API calls it may make, no call starts once `expired()` says the
/// request's deadline passed, and each call runs holding one of `slots`.
#[allow(clippy::too_many_arguments)]
pub async fn answer<B: Backends, S: CallSlots>(
    router: &Router<B>,
    secret: String,
    id: &str,
    auth: Authenticated,
    message: &Value,
    max_calls: usize,
    expired: &dyn Fn() -> bool,
    slots: &S,
) -> HttpResponse {
    let mut backend = RouterBackend {
        slots,
        router,
        secret,
        request_id: id.to_owned(),
        auth,
        steps: HashMap::new(),
        finished: HashMap::new(),
        calls: 0,
        max_calls,
        expired,
    };
    match runbook_mcp::handle_message_async(&mut backend, message, runbook_mcp::HTTP_PROTOCOLS)
        .await
    {
        Some(response) => json_response(200, id, &response),
        // A notification: accepted, nothing to answer.
        None => accepted(id),
    }
}

/// The whole exchange for a host that has the full body and no limits of
/// its own (a Workers `fetch` handler). `oauth` introspects OAuth access
/// tokens (MCP2); without it they are refused like any unknown bearer.
#[allow(clippy::too_many_arguments)]
pub async fn handle<B: Backends, I: crate::oauth::TokenIntrospector>(
    router: &Router<B>,
    oauth: Option<&I>,
    method: &str,
    headers: &Headers,
    body: &[u8],
    id: &str,
    max_calls: usize,
    expired: &dyn Fn() -> bool,
) -> HttpResponse {
    let origin = &router.config.discovery.origin;
    let enabled = oauth.is_some() && router.config.sign_in.is_some();
    with_challenge(
        Box::pin(exchange(
            router, oauth, method, headers, body, id, max_calls, expired,
        ))
        .await,
        origin,
        enabled,
    )
}

#[allow(clippy::too_many_arguments)]
async fn exchange<B: Backends, I: crate::oauth::TokenIntrospector>(
    router: &Router<B>,
    oauth: Option<&I>,
    method: &str,
    headers: &Headers,
    body: &[u8],
    id: &str,
    max_calls: usize,
    expired: &dyn Fn() -> bool,
) -> HttpResponse {
    let origin = &router.config.discovery.origin;
    let secret = match precheck(method, headers, origin, id) {
        Ok(secret) => secret,
        Err(response) => return response,
    };
    let authenticated = match oauth {
        // Only letters, digits and `_`: the host passes it on unescaped.
        Some(introspector)
            if crate::oauth::is_access_token(&secret)
                && secret
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
        {
            let looked_up = introspector
                .introspect(&secret, &crate::oauth::resource(origin))
                .await;
            authenticate_oauth(&router.backends, looked_up, id).await
        }
        _ => authenticate(&router.backends, &secret, id).await,
    };
    let auth = match authenticated {
        Ok(auth) => auth,
        Err(response) => return response,
    };
    if let Err(response) = declared_length(headers, id) {
        return response;
    }
    let message = match parse(body, id) {
        Ok(message) => message,
        Err(response) => return response,
    };
    answer(
        router, secret, id, auth, &message, max_calls, expired, &Unlimited,
    )
    .await
}

// --------------------------------------------------------------- backend

/// The tools' backend for one `/mcp` message. Host memory lasts for that
/// message only (the server is stateless); what the stdio host keeps across
/// calls is looked up instead: a step's execution, and whether an execution
/// credential's run has ended (it was revoked).
pub struct RouterBackend<'a, B, S> {
    router: &'a Router<B>,
    slots: &'a S,
    secret: String,
    request_id: String,
    auth: Authenticated,
    steps: HashMap<String, String>,
    finished: HashMap<String, bool>,
    calls: usize,
    max_calls: usize,
    expired: &'a dyn Fn() -> bool,
}

fn busy(code: &str) -> BackendError {
    BackendError::problem(503, code, code)
}

impl<B: Backends, S: CallSlots> RouterBackend<'_, B, S> {
    /// Count one API call against the message's budget and deadline, and
    /// build its request with the caller's bearer.
    fn admit(
        &mut self,
        method: &str,
        path: String,
        query: BTreeMap<String, String>,
        body: Value,
        idempotency_key: Option<String>,
        if_match: Option<String>,
    ) -> Result<Request, BackendError> {
        if self.calls >= self.max_calls {
            return Err(BackendError::problem(
                503,
                "limit.mcp_calls",
                format!(
                    "this message needed more than {} API calls; narrow the request",
                    self.max_calls
                ),
            ));
        }
        if (self.expired)() {
            return Err(busy("server.timeout"));
        }
        self.calls += 1;
        let nonce = nonce(&self.router.backends).ok_or_else(|| busy("server.entropy"))?;
        Ok(Request {
            method: method.into(),
            path,
            query,
            body,
            credential: Some(match &self.auth.oauth {
                Some(token) => Credential::Session(token.clone()),
                None => Credential::Bearer(self.secret.clone()),
            }),
            idempotency_key,
            request_id: self.request_id.clone(),
            new_id: nonce,
            if_match,
            client_ip: None,
        })
    }
}

impl<B: Backends, S: CallSlots> AsyncBackend for RouterBackend<'_, B, S> {
    async fn call(&mut self, call: HttpCall) -> Result<HttpReply, BackendError> {
        // Tools build only public API paths; anything else is refused here
        // as well, so no argument can reach a host-only route.
        if !call.path.starts_with("/v1/") || call.path.contains("/../") {
            return Err(BackendError::problem(
                404,
                "resource.not_found",
                "resource.not_found",
            ));
        }
        if crate::planned::recognizes(call.method, &call.path) {
            let (status, code) = crate::planned::NOT_IMPLEMENTED;
            return Err(BackendError::problem(status, code, code));
        }
        let mut request = self.admit(
            call.method,
            call.path,
            call.query.into_iter().collect(),
            call.body.unwrap_or(Value::Null),
            call.idempotency_key,
            call.if_match,
        )?;
        // This request was built by a server-side tool after MCP authentication.
        // Public HTTP decoding still requires the caller's CSRF header.
        if crate::runner_proxy::recognizes(&request.path) {
            let query = serde_urlencoded::to_string(&request.query).map_err(|_| {
                BackendError::problem(400, "request.invalid_query", "invalid query")
            })?;
            request.body = crate::runner_proxy::envelope(
                Some("1"),
                (!query.is_empty()).then_some(query.as_str()),
                request.body,
            );
        }
        let response = {
            let _slot = self.slots.acquire().await.map_err(busy)?;
            // Boxed: the router's future is large, and the tool's own future
            // would otherwise embed it (the worker's stack, and Workers', are
            // small).
            Box::pin(self.router.handle(request)).await
        };
        let body = match response.proxied {
            Some(proxy) if proxy.content_type == "application/json" && !proxy.bytes.is_empty() => {
                serde_json::from_slice(&proxy.bytes).map_err(|_| {
                    BackendError::problem(502, "runner.bad_response", "invalid JSON")
                })?
            }
            _ => response.body,
        };
        if (200..300).contains(&response.status) {
            Ok(HttpReply {
                body: match response.markdown {
                    Some(text) => Value::String(*text),
                    None => body,
                },
                etag: response.etag,
            })
        } else {
            Err(BackendError::from_problem(response.status, &body))
        }
    }

    fn random_token(&mut self) -> Result<String, BackendError> {
        let mut bytes = [0u8; 16];
        self.router
            .backends
            .fill_random(&mut bytes)
            .map_err(|_| busy("server.entropy"))?;
        Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    fn remember_step(&mut self, step_run_id: &str, execution_id: &str) {
        self.steps
            .insert(step_run_id.to_owned(), execution_id.to_owned());
    }

    async fn execution_of_step(&mut self, step_run_id: &str) -> Option<String> {
        if let Some(exe) = self.steps.get(step_run_id) {
            return Some(exe.clone());
        }
        // An execution credential is bound to one run.
        if let Some(exe) = &self.auth.execution {
            return Some(exe.clone());
        }
        let request = self
            .admit(
                "GET",
                PATH.into(),
                BTreeMap::from([("sr".to_owned(), step_run_id.to_owned())]),
                Value::Null,
                None,
                None,
            )
            .ok()?;
        let body = {
            let _slot = self.slots.acquire().await.ok()?;
            Box::pin(step_execution(&self.router.backends, &request))
                .await
                .ok()?
        };
        let exe = body["execution_id"].as_str()?.to_owned();
        self.steps.insert(step_run_id.to_owned(), exe.clone());
        Some(exe)
    }

    fn note_finished(&mut self, execution_id: &str, finished: bool) {
        self.finished.insert(execution_id.to_owned(), finished);
    }

    fn web_origin(&self) -> Option<String> {
        Some(
            self.router
                .config
                .discovery
                .origin
                .trim_end_matches('/')
                .to_owned(),
        )
    }

    fn finished(&mut self, execution_id: &str) -> bool {
        self.finished.get(execution_id).copied().unwrap_or(false)
            || (self.auth.ended && self.auth.execution.as_deref() == Some(execution_id))
    }
}

// --------------------------------------------------------------- lookups

type Failure = (u16, &'static str);

const UNAVAILABLE: Failure = (503, "storage.unavailable");
const INVALID: Failure = (401, "auth.invalid_credential");

/// A live personal or service credential's record.
async fn live_record<B: Backends>(backends: &B, r: &Request) -> Result<CredentialRecord, Failure> {
    let record = r
        .credential_record(&backends.credentials())
        .await
        .map_err(|e| e.status())?;
    let now = backends
        .directory()
        .current_time_ms()
        .await
        .map_err(|_| UNAVAILABLE)?;
    if record.validate().is_err()
        || record.revoked_at.is_some()
        || record.expires_at.is_some_and(|t| t <= now)
    {
        return Err(INVALID);
    }
    Ok(record)
}

/// Which kind of live credential the bearer is. PAT, service and execution
/// credentials only; a refresh token is never a bearer. A known execution
/// credential that was revoked is let through (`ended: true`) only when its
/// run is terminal: the closing commit revoked it, so its tools answer
/// `exit` rather than an HTTP challenge. No revocation reason is stored, so
/// a family revoked while the run is live (by a person, or on refresh
/// reuse) is an invalid token like any other.
pub async fn credential_kind<B: Backends>(backends: &B, r: &Request) -> Result<Value, Failure> {
    let secret = r.bearer().ok_or((401, "auth.required"))?;
    let (kind, _) = parse_secret(secret).map_err(|_| INVALID)?;
    match kind {
        TokenKind::Personal | TokenKind::Service => {
            live_record(backends, r).await?;
            Ok(json!({"kind": kind}))
        }
        TokenKind::Execution => {
            match crate::execution_credentials::access(backends, secret).await {
                Ok(access) => Ok(json!({
                    "kind": kind,
                    "execution_id": access.principal.execution_id,
                    "ended": false,
                })),
                Err(e) if e.code == "credentials.revoked" => {
                    let hash = secret_hash(secret);
                    let scope = backends
                        .execution_scope(&hash)
                        .await
                        .map_err(|_| UNAVAILABLE)?
                        .ok_or(INVALID)?;
                    let store = backends.execution(scope);
                    let record = store
                        .credential_by_hash(&hash)
                        .await
                        .map_err(|_| UNAVAILABLE)?
                        .ok_or(INVALID)?;
                    let run = store
                        .get_run(&record.execution_id)
                        .await
                        .map_err(|_| UNAVAILABLE)?
                        .ok_or(INVALID)?;
                    if run.completed_at_ms.is_none() {
                        return Err(INVALID);
                    }
                    Ok(json!({
                        "kind": kind,
                        "execution_id": record.execution_id,
                        "ended": true,
                    }))
                }
                Err(e) => Err((e.status, e.code)),
            }
        }
        TokenKind::Refresh => Err(INVALID),
    }
}

/// The run a step belongs to (`?sr=`), for a PAT or service credential that
/// may read executions in the step's project (the same check as reading
/// the run); anyone else gets 404, as for an unknown step. Used after the
/// caller's own write on the step (claim, report, release, decide), to read
/// that run's brief with the caller's bearer, which the brief route
/// authorizes again.
pub async fn step_execution<B: Backends>(backends: &B, r: &Request) -> Result<Value, Failure> {
    let not_found = (404, "resource.not_found");
    let step: runbook_model::StepRunId = r
        .query
        .get("sr")
        .and_then(|s| s.parse().ok())
        .ok_or(not_found)?;
    let record = live_record(backends, r).await?;
    let backend = crate::actor_store::StoreActorBackend {
        backends: backends.clone(),
    };
    let (execution, project) = backend
        .step_run_execution(&record.org_id, &step)
        .await
        .map_err(|e| (e.status, e.code))?
        .ok_or(not_found)?;
    let directory = backends.directory();
    let snapshot = directory
        .snapshot(&record.org_id)
        .await
        .map_err(|_| UNAVAILABLE)?;
    let now = directory.current_time_ms().await.map_err(|_| UNAVAILABLE)?;
    authorize(&record, &snapshot, now, &project, Scope::ExecutionsRead).map_err(|_| not_found)?;
    Ok(json!({"execution_id": execution}))
}

/// `Response` from a lookup's outcome (tests and hosts that route them).
pub fn lookup_response(outcome: Result<Value, Failure>, id: &str) -> Response {
    match outcome {
        Ok(body) => Response::json(200, body, id),
        Err((status, code)) => Response::error(status, code, id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn h(pairs: &[(&str, &str)]) -> Headers {
        pairs.iter().copied().collect()
    }
    #[test]
    fn prechecks_run_in_order_before_any_work() {
        let o = "https://runbooks.example";
        let ok = [
            ("authorization", "Bearer trb_pat_x"),
            ("content-type", "application/json"),
        ];
        assert_eq!(precheck("POST", &h(&ok), o, "req").unwrap(), "trb_pat_x");
        let status = |method, pairs: &[(&str, &str)]| {
            precheck(method, &h(pairs), o, "req").unwrap_err().status
        };
        assert_eq!(status("POST", &[("origin", "https://evil.example")]), 403);
        assert_eq!(status("GET", &ok), 405);
        assert_eq!(status("POST", &[("content-type", "application/json")]), 401);
        let mut no_json = ok.to_vec();
        no_json.push(("accept", "text/event-stream"));
        assert_eq!(status("POST", &no_json), 406);
        let mut old = ok.to_vec();
        old.push(("mcp-protocol-version", "2024-11-05"));
        assert_eq!(status("POST", &old), 400);
        assert_eq!(status("POST", &[("authorization", "Bearer x")]), 415);
        let allow = precheck("GET", &h(&ok), o, "req").unwrap_err();
        assert_eq!(allow.header_value("allow"), Some("POST"));
    }
    #[test]
    fn messages_are_single_objects_with_a_method() {
        assert!(parse(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, "r").is_ok());
        assert_eq!(parse(b"[]", "r").unwrap_err().status, 400);
        assert_eq!(parse(b"{", "r").unwrap_err().status, 400);
        assert_eq!(
            parse(br#"{"id":1,"result":{}}"#, "r").unwrap_err().status,
            202
        );
    }
}
