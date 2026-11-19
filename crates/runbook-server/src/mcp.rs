//! MCP1 on this host (CF0): `/mcp` is portable (`runbook_api::mcp`); this
//! adapter adds the server's limits around it. Messages in flight are
//! bounded (`mcp_permits`), the body counts against the shared budget, and
//! the message runs on the worker thread, where the tools call the API
//! router in process (no blocking threads). The deadline stops new API
//! calls; a reply that misses it answers 503 `server.timeout`.
use crate::http::Client;
use axum::{
    body::{Body, to_bytes},
    http::{Request, Response},
};
use runbook_api::{
    ResolvedSession,
    http::HttpResponse,
    mcp::{Authenticated, refusal},
    session::Unavailable,
};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

pub(crate) use runbook_api::mcp::PATH;

fn new_id(prefix: &str) -> Option<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())?;
    crate::bootstrap::id(prefix, now).ok()
}

/// `/mcp` work for the worker thread.
pub(crate) enum McpWork {
    Authenticate {
        secret: String,
        id: String,
        /// MCP2: an OAuth access token's introspection, done by the
        /// transport (it may block on the auth child); linking runs here.
        oauth: Option<Result<Option<ResolvedSession>, Unavailable>>,
        reply: oneshot::Sender<Result<Authenticated, HttpResponse>>,
    },
    Message {
        secret: String,
        id: String,
        auth: Authenticated,
        message: Value,
        /// No API call starts after it.
        deadline: Instant,
        max_calls: usize,
        /// Held until the message ends, even past the request timeout.
        permit: OwnedSemaphorePermit,
        reply: oneshot::Sender<HttpResponse>,
    },
}

impl McpWork {
    /// A message runs without a worker slot of its own; its calls take one
    /// each (`WorkerSlots`).
    pub(crate) fn is_message(&self) -> bool {
        matches!(self, Self::Message { .. })
    }
}

/// CF0 review: each API call of a message holds one worker slot (the same
/// `max_in_flight` permits REST takes) for that call only, as each tool call
/// did before CF0. A call that cannot get one before the message's
/// deadline fails with `server.timeout`.
struct WorkerSlots {
    permits: Arc<Semaphore>,
    deadline: Instant,
}
impl runbook_api::mcp::CallSlots for WorkerSlots {
    type Slot = OwnedSemaphorePermit;
    async fn acquire(&self) -> Result<OwnedSemaphorePermit, &'static str> {
        match tokio::time::timeout_at(self.deadline.into(), self.permits.clone().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err("server.unavailable"),
            Err(_) => Err("server.timeout"),
        }
    }
}

/// Run `/mcp` work on the worker thread. Authentication is one short call
/// (the caller holds a worker slot for it); a message holds its `/mcp`
/// permit, and its calls take worker slots from `permits` one at a time.
pub(crate) async fn run<B: runbook_api::backends::Backends>(
    router: &runbook_api::router::Router<B>,
    work: McpWork,
    permits: Arc<Semaphore>,
) {
    match work {
        McpWork::Authenticate {
            secret,
            id,
            oauth,
            reply,
        } => {
            let answer = match oauth {
                Some(looked_up) => {
                    runbook_api::mcp::authenticate_oauth(&router.backends, looked_up, &id).await
                }
                None => runbook_api::mcp::authenticate(&router.backends, &secret, &id).await,
            };
            let _ = reply.send(answer);
        }
        McpWork::Message {
            secret,
            id,
            auth,
            message,
            deadline,
            max_calls,
            permit,
            reply,
        } => {
            let expired = move || Instant::now() >= deadline;
            let slots = WorkerSlots { permits, deadline };
            let response = runbook_api::mcp::answer(
                router, secret, &id, auth, &message, max_calls, &expired, &slots,
            )
            .await;
            // The work is done: free the slot before answering, so a client
            // that sends its next message on the reply finds it free.
            drop(permit);
            let _ = reply.send(response);
        }
    }
}

/// `/mcp`: the exchange, with every 401 naming the protected resource
/// metadata when sign-in (the authorization server) is configured (MCP2).
pub(crate) async fn handle(client: Client, request: Request<Body>) -> Response<Body> {
    let origin = client.origin.clone();
    let oauth = client.auth.is_some();
    crate::http::to_axum(runbook_api::mcp::with_challenge(
        exchange(client, request).await,
        &origin,
        oauth,
    ))
}

async fn exchange(client: Client, request: Request<Body>) -> HttpResponse {
    let encode = std::convert::identity::<HttpResponse>;
    let Some(id) = new_id("req_") else {
        return encode(refusal(
            503,
            "req_unavailable",
            -32603,
            "server.entropy",
            "Unavailable",
        ));
    };
    let (parts, body) = request.into_parts();
    let headers = crate::http::headers(&parts.headers);
    let secret =
        match runbook_api::mcp::precheck(parts.method.as_str(), &headers, &client.origin, &id) {
            Ok(secret) => secret,
            Err(response) => return encode(response),
        };
    // Messages in flight are bounded; a full house answers at once. The
    // permit moves into the message's task, so work that outlives the
    // request timeout still counts until it ends.
    let Ok(permit) = client.mcp_permits.clone().try_acquire_owned() else {
        return encode(refusal(503, &id, -32603, "server.busy", "Busy; retry"));
    };
    let deadline = Instant::now() + client.request_timeout;
    // MCP2: an OAuth access token is introspected through the auth child
    // (cached 30 s); without sign-in it is unknown, like any bad bearer.
    let oauth = if runbook_api::oauth::is_access_token(&secret) {
        Some(match &client.auth {
            Some(sessions) => {
                let resource = runbook_api::oauth::resource(&client.origin);
                sessions.introspect(&secret, &resource).await
            }
            None => Ok(None),
        })
    } else {
        None
    };
    let (reply, receive) = oneshot::channel();
    let work = McpWork::Authenticate {
        secret: secret.clone(),
        id: id.clone(),
        oauth,
        reply,
    };
    let auth = match client.submit_mcp(work) {
        Ok(()) => match tokio::time::timeout(client.request_timeout, receive).await {
            Ok(Ok(Ok(auth))) => auth,
            Ok(Ok(Err(response))) => return encode(response),
            Err(_) => {
                return encode(refusal(
                    503,
                    &id,
                    -32603,
                    "server.timeout",
                    "Timed out; retry",
                ));
            }
            Ok(Err(_)) => {
                return encode(refusal(
                    503,
                    &id,
                    -32603,
                    "server.unavailable",
                    "Unavailable; retry",
                ));
            }
        },
        Err(()) => {
            return encode(refusal(
                503,
                &id,
                -32603,
                "server.unavailable",
                "Unavailable; retry",
            ));
        }
    };
    // F45: the JSON body limit and the shared body budget, as on /v1.
    let limit = client.json_body_bytes;
    let declared = match runbook_api::mcp::declared_length(&headers, &id) {
        Ok(declared) => declared,
        Err(response) => return encode(response),
    };
    if declared.is_some_and(|n| n > limit as u64) {
        return encode(runbook_api::mcp::too_large(&id));
    }
    let reserve = declared.map_or(limit, |n| n as usize).div_ceil(1024).max(1);
    let Some(_budget) = u32::try_from(reserve)
        .ok()
        .and_then(|n| client.body_budget.clone().try_acquire_many_owned(n).ok())
    else {
        return encode(refusal(503, &id, -32603, "server.busy", "Busy; retry"));
    };
    let Ok(bytes) = to_bytes(body, limit).await else {
        return encode(runbook_api::mcp::too_large(&id));
    };
    let message = match runbook_api::mcp::parse(&bytes, &id) {
        Ok(message) => message,
        Err(response) => return encode(response),
    };
    let (reply, receive) = oneshot::channel();
    let work = McpWork::Message {
        secret,
        id: id.clone(),
        auth,
        message,
        deadline,
        max_calls: client.mcp_calls,
        permit,
        reply,
    };
    if client.submit_mcp(work).is_err() {
        return encode(refusal(503, &id, -32603, "server.busy", "Busy; retry"));
    }
    match tokio::time::timeout(client.request_timeout, receive).await {
        Ok(Ok(response)) => encode(response),
        Ok(Err(_)) => encode(refusal(
            500,
            &id,
            -32603,
            "server.internal",
            "Internal error",
        )),
        Err(_) => encode(refusal(
            503,
            &id,
            -32603,
            "server.timeout",
            "Timed out; retry with the same idempotency_key",
        )),
    }
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
