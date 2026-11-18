//! Send transport passes owned messages to a bounded, non-Send local executor.
use crate::{Error, config::ServerConfig};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, Response, header},
};
use runbook_api::problem::Response as ApiResponse;
use runbook_store_postgres::{PostgresCredentials, PostgresDirectory};
use std::{
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot, watch};
struct Call {
    request: runbook_api::Request,
    /// The request passed the `/internal/*` gate (loopback and the
    /// per-boot secret): the auth child's registration hook.
    internal: bool,
    /// AUTH3/HOST1: a device-code request's per-IP rate-limit key, counted
    /// by the worker in the shared windows before the route runs.
    limit: Option<String>,
    reply: oneshot::Sender<ApiResponse>,
}
/// D50: one request from the user-content listener.
struct FileCall {
    request: runbook_api::evidence::UserContentRequest,
    reply: oneshot::Sender<runbook_api::evidence::UserContentReply>,
}
/// What the local executor takes next.
enum Work {
    Api(Call),
    Files(FileCall),
    /// MCP1: a `/mcp` message (or its authentication), run in process.
    Mcp(crate::mcp::McpWork),
}
#[derive(Clone)]
pub struct Client {
    tx: mpsc::Sender<Call>,
    streams: mpsc::Sender<crate::event_stream::StreamCall>,
    max_request_bytes: usize,
    pub(crate) json_body_bytes: usize,
    pub(crate) request_timeout: Duration,
    /// MCP1: permits for `/mcp` messages in flight.
    pub(crate) mcp_permits: Arc<tokio::sync::Semaphore>,
    /// MCP1: API calls one `/mcp` message may make.
    pub(crate) mcp_calls: usize,
    /// F45: body bytes buffered at once, in KiB permits.
    pub(crate) body_budget: Arc<tokio::sync::Semaphore>,
    /// B2: the built web UI, when RUNBOOK_WEB_DIR is set.
    web: Option<Arc<crate::web_assets::WebAssets>>,
    /// AUTH2: sessions and the auth child, when RUNBOOK_AUTH_SECRET is set.
    pub(crate) auth: Option<Arc<crate::session::Sessions>>,
    /// Sign-in gate fix 1: RUNBOOK_TRUSTED_PROXIES.
    trusted_proxies: Arc<crate::client_ip::TrustedProxies>,
    /// UP2 (D50): requests from the user-content listener.
    files: mpsc::Sender<FileCall>,
    /// MCP1: `/mcp` work for the worker.
    mcp: mpsc::Sender<crate::mcp::McpWork>,
    /// RUNBOOK_ORIGIN: the only origin the signed PUT allows cross-origin,
    /// and the only `Origin` `/mcp` accepts (MCP1, DNS rebinding).
    pub(crate) origin: Arc<str>,
    /// Time allowed for one signed PUT, body included.
    upload_timeout: Duration,
    /// UP2 fixes: the user-content listener's own body budget (KiB
    /// permits), so uploads never take the API's.
    files_budget: Arc<tokio::sync::Semaphore>,
    /// UP2 fixes: one image in the pipeline at a time.
    image_slot: crate::backends::ImageSlot,
    /// The upload signing key, when uploads are enabled (D50): the
    /// user-content listener checks signatures before any database call.
    upload_key: Option<runbook_core::uploads::signing::UploadSigningKey>,
    /// HOST1: readiness probes, answered by the worker with one pooled
    /// `SELECT 1`.
    pings: mpsc::Sender<oneshot::Sender<bool>>,
    /// HOST1 review: the last readiness answer and when it was taken. The
    /// async lock makes the ping single-flight: probes arriving while one
    /// runs wait for it and reuse its answer.
    readiness: Arc<tokio::sync::Mutex<Option<(std::time::Instant, bool)>>>,
    /// HOST1: true once shutdown began: readiness answers 503 and open
    /// event streams end (clients resume elsewhere with Last-Event-ID).
    shutdown: watch::Receiver<bool>,
}
impl Client {
    /// Queue `/mcp` work on the worker (backpressure: `Err` when full).
    pub(crate) fn submit_mcp(&self, work: crate::mcp::McpWork) -> Result<(), ()> {
        self.mcp.try_send(work).map_err(|_| ())
    }
    /// The image pipeline's slot (tests hold it to see finalize answer
    /// `server.busy`).
    #[doc(hidden)]
    pub fn image_slot(&self) -> Arc<tokio::sync::Semaphore> {
        self.image_slot.clone()
    }
    /// AUTH2: the supervised auth child, when sign-in is configured.
    pub fn auth_child(&self) -> Option<&crate::auth_child::AuthChild> {
        self.auth.as_deref().map(crate::session::Sessions::child)
    }
    /// HOST1: shutdown has begun.
    pub fn shutting_down(&self) -> bool {
        *self.shutdown.borrow()
    }
    /// HOST1: whether the database answers, through the worker's pool:
    /// one ping at a time, its answer reused for [`READY_CACHE`].
    pub async fn database_ready(&self, timeout: Duration) -> bool {
        let mut last = self.readiness.lock().await;
        if let Some((at, ok)) = *last
            && at.elapsed() < READY_CACHE
        {
            return ok;
        }
        let (reply, answer) = oneshot::channel();
        let ok = self.pings.try_send(reply).is_ok()
            && matches!(tokio::time::timeout(timeout, answer).await, Ok(Ok(true)));
        *last = Some((std::time::Instant::now(), ok));
        ok
    }
}
/// The connection's peer address, set per connection by [`serve_on`].
/// Requests built without one (in-process tests) have no peer: they are
/// never loopback for the internal routes and share one rate-limit bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerAddr(pub std::net::SocketAddr);
fn peer(request: &Request<Body>) -> Option<std::net::IpAddr> {
    request.extensions().get::<PeerAddr>().map(|p| p.0.ip())
}
/// Sign-in gate fix 1: the client's address, through any trusted proxy
/// ([`crate::client_ip`]). Per-IP limits and `x-runbook-client-ip` use it;
/// the internal-route gate uses the TCP peer itself.
fn client_ip(client: &Client, request: &Request<Body>) -> Option<std::net::IpAddr> {
    client
        .trusted_proxies
        .client_ip(peer(request), request.headers())
}
/// Where the auth child reaches this server's `/internal/*` routes: the
/// listen address, with an unspecified address replaced by loopback.
fn internal_url(listen: std::net::SocketAddr) -> String {
    let host = match listen.ip() {
        std::net::IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        std::net::IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_owned(),
        std::net::IpAddr::V4(ip) => ip.to_string(),
        std::net::IpAddr::V6(ip) => format!("[{ip}]"),
    };
    format!("http://{host}:{}", listen.port())
}
/// HOST1 review: how long one readiness answer is reused.
pub const READY_CACHE: Duration = Duration::from_secs(1);
/// HOST1 review: the longest one readiness ping may run in the worker
/// (acquiring a connection included); it never outlives the probe.
const PING_TIMEOUT: Duration = Duration::from_secs(3);

/// The worker thread's stack (CF0 review 3).
const WORKER_STACK_BYTES: usize = 8 << 20;
pub struct Worker {
    client: Client,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    shutdown: watch::Sender<bool>,
}
impl Worker {
    pub fn start(config: ServerConfig) -> Result<Self, Error> {
        config.retention.check()?;
        let limits = config.transport.clone();
        let (tx, mut rx) = mpsc::channel::<Call>(limits.queue_depth.max(1));
        let (files, mut files_rx) = mpsc::channel::<FileCall>(limits.queue_depth.max(1));
        let (mcp, mut mcp_rx) = mpsc::channel::<crate::mcp::McpWork>(limits.queue_depth.max(1));
        let (pings, mut ping_rx) = mpsc::channel::<oneshot::Sender<bool>>(8);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let origin: Arc<str> = config.origin.as_str().into();
        let image_slot: crate::backends::ImageSlot = Arc::new(tokio::sync::Semaphore::new(1));
        let worker_image_slot = image_slot.clone();
        let upload_key = config
            .uploads
            .enabled(&config.origin)
            .then(|| config.uploads.signing_key.clone())
            .flatten();
        let (streams, mut stream_rx) = mpsc::channel::<crate::event_stream::StreamCall>(32);
        let (stop, mut stopped) = oneshot::channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error("server.runtime"))?;
        let max_request_bytes = config.max_request_bytes;
        let web = match &config.web_dir {
            Some(dir) => Some(Arc::new(crate::web_assets::WebAssets::open(dir)?)),
            None => None,
        };
        let auth = match &config.auth {
            Some(auth) => Some(crate::session::Sessions::new(
                crate::auth_child::AuthChild::start(
                    auth,
                    &internal_url(config.listen),
                    &config.origin,
                )?,
                auth.clone(),
                &config.origin,
            )),
            None => None,
        };
        let worker_auth = auth.clone();
        let router_config = crate::backends::router_config(&config);
        let thread = std::thread::Builder::new()
            .name("runbook-local-executor".into())
            // CF0 review 3: every request's poll chain runs here (an `/mcp`
            // message calls the router in process); an overflow would abort
            // the server, so the stack is explicit, not the 2 MiB default.
            .stack_size(WORKER_STACK_BYTES)
            .spawn(move || {
                runtime.block_on(async move {
                    let local = tokio::task::LocalSet::new();
                    let backends = crate::backends::PostgresBackends::new(
                        &config,
                        worker_image_slot.clone(),
                        worker_auth.clone(),
                    );
                    // CF0: one portable router serves every API request.
                    let router = Rc::new(runbook_api::router::Router::new(
                        backends.clone(),
                        router_config,
                    ));
                    let executions = runbook_api::executions::ExecutionService {
                        directory: PostgresDirectory::new(config.database.clone()),
                        credentials: PostgresCredentials::new(config.database.clone()),
                        backend: crate::executions::PostgresExecutionBackend {
                            config: config.database.clone(),
                            objects: backends.objects.clone(),
                        },
                        origin: config.origin.clone(),
                    };
                    // Backpressure only. Database revision locks arbitrate every mutation.
                    let permits = Arc::new(tokio::sync::Semaphore::new(
                        config.transport.max_in_flight.max(1),
                    ));
                    let streams = crate::event_stream::Streams::new(
                        executions,
                        config.database.clone(),
                        &config.transport,
                        worker_auth.clone(),
                        backends.clone(),
                    );
                    let ping_config = config.database.clone();
                    tokio::select! {
                        _ = &mut stopped => {},
                        _ = local.run_until(async move {
                            let admit_stream = |call: crate::event_stream::StreamCall| streams.admit(call);
                            loop {
                                let work = tokio::select! {
                                    ping = ping_rx.recv() => {
                                        let Some(reply) = ping else { break; };
                                        let config = ping_config.clone();
                                        tokio::task::spawn_local(async move {
                                            let ok = tokio::time::timeout(
                                                PING_TIMEOUT,
                                                runbook_store_postgres::ping(&config),
                                            )
                                            .await
                                            .is_ok_and(|r| r.is_ok());
                                            let _ = reply.send(ok);
                                        });
                                        continue;
                                    }
                                    call = rx.recv() => match call { Some(call) => Work::Api(call), None => break },
                                    call = files_rx.recv() => match call { Some(call) => Work::Files(call), None => break },
                                    work = mcp_rx.recv() => match work { Some(work) => Work::Mcp(work), None => break },
                                    stream = stream_rx.recv() => {
                                        let Some(call) = stream else { break; };
                                        admit_stream(call);
                                        continue;
                                    }
                                };
                                // A `/mcp` message takes no worker slot of its own;
                                // each of its API calls takes one (`mcp::run`).
                                let work = match work {
                                    Work::Mcp(work) if work.is_message() => {
                                        let router = router.clone();
                                        let permits = permits.clone();
                                        tokio::task::spawn_local(async move {
                                            crate::mcp::run(&router, work, permits).await;
                                        });
                                        continue;
                                    }
                                    work => work,
                                };
                                // Streams are admitted (or refused) while requests wait for a slot.
                                let permit = loop {
                                    tokio::select! {
                                        permit = permits.clone().acquire_owned() => break permit,
                                        Some(stream) = stream_rx.recv() => admit_stream(stream),
                                    }
                                };
                                let Ok(permit) = permit else { break; };
                                let router = router.clone();
                                match work {
                                    Work::Files(call) => {
                                        tokio::task::spawn_local(async move {
                                            let _permit = permit;
                                            let reply = runbook_api::evidence::usercontent(
                                                &router.backends,
                                                &router.config.uploads,
                                                call.request,
                                            )
                                            .await;
                                            let _ = call.reply.send(reply);
                                        });
                                    }
                                    Work::Mcp(work) => {
                                        let permits = permits.clone();
                                        tokio::task::spawn_local(async move {
                                            let _permit = permit;
                                            crate::mcp::run(&router, work, permits).await;
                                        });
                                    }
                                    Work::Api(call) => {
                                        tokio::task::spawn_local(async move {
                                            let _permit = permit;
                                            if let Some(key) = &call.limit {
                                                use runbook_api::backends::Backends;
                                                if let Err(limited) = router
                                                    .backends
                                                    .rate_limit(
                                                        key,
                                                        crate::device::CODE_WINDOW_MS,
                                                        crate::device::CODES_PER_HOUR,
                                                    )
                                                    .await
                                                {
                                                    let _ = call
                                                        .reply
                                                        .send(limited.response(&call.request.request_id));
                                                    return;
                                                }
                                            }
                                            let response = if call.internal {
                                                router.can_register(call.request).await
                                            } else {
                                                router.handle(call.request).await
                                            };
                                            let _ = call.reply.send(response);
                                        });
                                    }
                                }
                            }
                        }) => {}
                    }
                });
            })
            .map_err(|_| Error("server.worker"))?;
        Ok(Self {
            client: Client {
                tx,
                streams,
                max_request_bytes,
                json_body_bytes: limits.json_body_bytes.min(max_request_bytes),
                request_timeout: limits.request_timeout,
                mcp_permits: Arc::new(tokio::sync::Semaphore::new(limits.mcp_in_flight)),
                mcp_calls: limits.mcp_calls_per_message,
                body_budget: Arc::new(tokio::sync::Semaphore::new(
                    (limits.in_flight_body_bytes / 1024).min(tokio::sync::Semaphore::MAX_PERMITS),
                )),
                web,
                auth,
                trusted_proxies: Arc::new(limits.trusted_proxies.clone()),
                files,
                mcp,
                origin,
                upload_timeout: limits.upload_timeout,
                files_budget: Arc::new(tokio::sync::Semaphore::new(
                    (limits.in_flight_body_bytes / 1024).min(tokio::sync::Semaphore::MAX_PERMITS),
                )),
                image_slot,
                upload_key,
                pings,
                readiness: Arc::default(),
                shutdown: shutdown_rx,
            },
            stop: Some(stop),
            thread: Some(thread),
            shutdown,
        })
    }
    pub fn client(&self) -> Client {
        self.client.clone()
    }
    /// HOST1: readiness answers 503 from now on and open event streams end.
    pub fn begin_shutdown(&self) {
        self.shutdown.send_replace(true);
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub fn router(client: Client) -> Router {
    Router::new()
        .fallback(dispatch)
        .with_state(client)
        .layer(axum::middleware::map_response(no_sniff))
}
/// F96: every response, including the Markdown export and event streams,
/// forbids MIME sniffing.
async fn no_sniff(mut response: Response<Body>) -> Response<Body> {
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        http::HeaderValue::from_static("nosniff"),
    );
    response
}
/// HOST1: liveness, answered without the database.
pub const LIVE_PATH: &str = "/health/live";
/// HOST1: readiness: not shutting down, and the database answers.
pub const READY_PATH: &str = "/health/ready";

/// A probe request: which one, and whether it is HEAD; `Err` for another
/// method.
type Probe = Result<(bool, bool), ()>;
fn probe(method: &http::Method, path: &str) -> Option<Probe> {
    let ready = match path {
        LIVE_PATH => false,
        READY_PATH => true,
        _ => return None,
    };
    Some(match *method {
        http::Method::GET => Ok((ready, false)),
        http::Method::HEAD => Ok((ready, true)),
        _ => Err(()),
    })
}

/// HOST1 probes. Paths end in no `z` (Cloud Run reserves some that do), and
/// the bodies name nothing but a status.
async fn health(client: &Client, probe: Probe) -> Response<Body> {
    let Ok((ready, head)) = probe else {
        return encode(ApiResponse::error(
            405,
            "request.method_not_allowed",
            "req_health",
        ));
    };
    let (status, word) = if !ready {
        (200, "live")
    } else if client.shutting_down() {
        (503, "stopping")
    } else if client.database_ready(Duration::from_secs(3)).await {
        (200, "ready")
    } else {
        (503, "database_unavailable")
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(if head {
            Body::empty()
        } else {
            Body::from(serde_json::json!({ "status": word }).to_string())
        })
        .expect("fixed health response")
}

async fn dispatch(State(client): State<Client>, request: Request<Body>) -> Response<Body> {
    if let Some(probe) = probe(request.method(), request.uri().path()) {
        return health(&client, probe).await;
    }
    // AUTH2 (D43): sign-in belongs to the auth child, and (MCP2) so do the
    // OAuth discovery documents. Without one, these paths are the API's
    // unknown routes.
    if let Some(auth) = &client.auth
        && (crate::auth_proxy::recognizes(request.uri().path())
            || runbook_api::oauth::proxied(request.uri().path()))
    {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .and_then(|now| crate::bootstrap::id("req_", now).ok())
            .unwrap_or_else(|| "req_unavailable".into());
        let ip = client_ip(&client, &request);
        return crate::auth_proxy::forward(auth.clone(), request, ip, &id).await;
    }
    // MCP1: the remote MCP endpoint answers every method itself, so the web
    // UI never serves its SPA shell there.
    if request.uri().path() == crate::mcp::PATH {
        return crate::mcp::handle(client, request).await;
    }
    // B2: GET/HEAD outside the API prefixes may be the web UI; anything it
    // declines (API paths, refused or missing assets) is the API's to answer.
    if let Some(web) = &client.web
        && let Some(response) = web
            .respond(request.method(), request.uri().path(), request.headers())
            .await
    {
        return response;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .and_then(|now| crate::bootstrap::id("", now).ok());
    let Some(nonce) = nonce else {
        return encode(ApiResponse::error(503, "server.entropy", "req_unavailable"));
    };
    let request_id = format!("req_{nonce}");
    // F52: only the exact events route streams; `.../signals/events` is a POST.
    if runbook_api::directory::match_path("/v1/executions/{exe}/events", request.uri().path())
        .is_some()
    {
        return stream_response(client, request, nonce, &request_id).await;
    }
    let response = tokio::time::timeout(
        client.request_timeout,
        process(client, request, nonce, &request_id),
    )
    .await
    .unwrap_or_else(|_| ApiResponse::error(503, "server.timeout", &request_id));
    encode(response)
}
/// CF0: the request's headers in the portable shape.
pub(crate) fn headers(map: &http::HeaderMap) -> runbook_api::http::Headers {
    map.iter()
        .map(|(name, value)| (name.as_str(), value.as_bytes()))
        .collect()
}
/// D46: how a request authenticates (`runbook_api::session::credential`).
async fn credential(
    client: &Client,
    method: &http::Method,
    path: &str,
    map: &http::HeaderMap,
    id: &str,
) -> Result<(Option<runbook_api::Credential>, Option<String>), ApiResponse> {
    runbook_api::session::credential(
        client.auth.as_deref(),
        method.as_str(),
        path,
        &headers(map),
        id,
    )
    .await
}
async fn process(client: Client, request: Request<Body>, nonce: String, id: &str) -> ApiResponse {
    let peer = peer(&request);
    let ip = client_ip(&client, &request);
    let (parts, body) = request.into_parts();
    // Later-plans rule 5: a planned operation answers 501 before any
    // authentication or body byte, whoever calls it.
    if runbook_api::planned::recognizes(parts.method.as_str(), parts.uri.path()) {
        let (status, code) = runbook_api::planned::NOT_IMPLEMENTED;
        return ApiResponse::error(status, code, id);
    }
    // AUTH2: `/internal/*` exists only for the auth child on loopback with
    // the per-boot secret; to anyone else it is an unknown route.
    let internal = parts.uri.path().starts_with("/internal/");
    if internal
        && (parts.uri.path() != crate::session::CAN_REGISTER
            || !crate::session::internal_allowed(client.auth.as_deref(), &parts.headers, peer))
    {
        return ApiResponse::error(404, "resource.not_found", id);
    }
    let (credential, _) = if internal {
        (None, None)
    } else {
        match credential(&client, &parts.method, parts.uri.path(), &parts.headers, id).await {
            Ok(found) => found,
            Err(refused) => return refused,
        }
    };
    // F45: decide from the headers alone, before any body byte is read.
    let path = parts.uri.path();
    let headers = headers(&parts.headers);
    let plan = match runbook_api::http::plan_body(parts.method.as_str(), path, &headers, id) {
        Ok(plan) => plan,
        Err(refused) => return refused,
    };
    let limit = if plan.upload {
        client.max_request_bytes
    } else {
        client.json_body_bytes
    };
    // AUTH3: device codes are limited per client address (only where
    // sign-in exists); the worker counts the hit in the shared windows.
    let rate_key = (plan.device_code && client.auth.is_some()).then(|| crate::device::code_key(ip));
    if plan.needs_credential && credential.is_none() && !internal {
        return ApiResponse::error(401, "auth.required", id);
    }
    if plan.declared.is_some_and(|n| n > limit as u64) {
        return ApiResponse::error(413, "request.body_too_large", id);
    }
    let _budget = if plan.has_body {
        let reserve = plan
            .declared
            .map_or(limit, |n| n as usize)
            .div_ceil(1024)
            .max(1);
        match u32::try_from(reserve)
            .ok()
            .and_then(|n| client.body_budget.clone().try_acquire_many_owned(n).ok())
        {
            Some(permit) => Some(permit),
            None => return ApiResponse::error(503, "server.busy", id),
        }
    } else {
        None
    };
    let bytes = match to_bytes(body, limit).await {
        Ok(b) => b,
        Err(_) => return ApiResponse::error(413, "request.body_too_large", id),
    };
    let request = match runbook_api::http::decode(
        parts.method.as_str(),
        path,
        parts.uri.query(),
        &headers,
        &bytes,
        credential,
        id,
        nonce,
    ) {
        Ok(mut request) => {
            // Sign-in gate fix 3: device codes record who asked.
            request.client_ip = ip.map(|ip| ip.to_string());
            request
        }
        Err(refused) => return refused,
    };
    let (reply, receive) = oneshot::channel();
    if client
        .tx
        .try_send(Call {
            request,
            internal,
            limit: rate_key,
            reply,
        })
        .is_err()
    {
        return ApiResponse::error(503, "server.busy", id);
    }
    receive
        .await
        .unwrap_or_else(|_| ApiResponse::error(503, "server.unavailable", id))
}
/// CF0: a portable response on the wire.
pub(crate) fn to_axum(response: runbook_api::http::HttpResponse) -> Response<Body> {
    let mut builder = Response::builder().status(response.status);
    for (name, value) in &response.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder
        .body(if response.body.is_empty() {
            Body::empty()
        } else {
            Body::from(response.body)
        })
        .unwrap_or_else(|_| {
            Response::builder()
                .status(500)
                .body(Body::empty())
                .expect("fixed response")
        })
}
/// The API's response shapes (`runbook_api::http::encode`).
fn encode(response: ApiResponse) -> Response<Body> {
    to_axum(runbook_api::http::encode(response))
}
/// HOST1: the listeners `serve` binds, after migrating the database (under
/// its advisory lock, so replicas starting together apply each migration
/// once and the rest wait).
pub struct Listeners {
    pub main: tokio::net::TcpListener,
    /// D50: the user-content origin's own listener, when it has one.
    pub files: Option<tokio::net::TcpListener>,
}
impl Listeners {
    /// Also records the database's schema in `config` (UPD1: `/v1/meta`
    /// reports it).
    pub async fn bind(config: &mut ServerConfig) -> Result<Self, Error> {
        let mut store = runbook_store_postgres::PostgresStore::connect(&config.database)
            .await
            .map_err(|e| Error(e.0))?;
        config.schema_version = if config.migrate_on_start {
            store.migrate().await.map_err(|e| Error(e.0))?
        } else {
            store.check_schema().await.map_err(|e| Error(e.0))?
        };
        drop(store);
        if config.schema_version > runbook_store_postgres::SCHEMA_VERSION {
            // UPD1: only with RUNBOOK_ALLOW_NEWER_SCHEMA (the rollback path).
            crate::log::warning(
                "server",
                "postgres.schema_newer_than_code: the database is at a newer release's schema (allowed by RUNBOOK_ALLOW_NEWER_SCHEMA)",
            );
        }
        let main = tokio::net::TcpListener::bind(config.listen)
            .await
            .map_err(|_| Error("server.listen"))?;
        let files = match (
            &config.uploads.usercontent_origin,
            config.uploads.usercontent_listen,
        ) {
            (Some(_), Some(address)) => Some(
                tokio::net::TcpListener::bind(address)
                    .await
                    .map_err(|_| Error("server.usercontent_listen"))?,
            ),
            _ => None,
        };
        Ok(Self { main, files })
    }
}

/// Migrate, bind and serve the API (and any user-content listener) until
/// the process ends, without background workers or signal handling. Tests
/// use it; `runbook-server serve` is [`crate::lifecycle::run`].
pub async fn serve(mut config: ServerConfig) -> Result<(), Error> {
    let listeners = Listeners::bind(&mut config).await?;
    let limits = config.transport.clone();
    let worker = Worker::start(config.clone())?;
    let main = serve_on(
        listeners.main,
        main_router(worker.client(), &config),
        &limits,
    );
    match listeners.files {
        Some(files) => {
            tokio::try_join!(
                main,
                serve_on(files, usercontent_router(worker.client()), &limits)
            )?;
            Ok(())
        }
        None => main.await,
    }
}

/// The main listener's app: the API, or (HOST1, single-port mode) the API
/// and the user-content origin routed by `Host`.
pub fn main_router(client: Client, config: &ServerConfig) -> Router {
    match config
        .uploads
        .usercontent_origin
        .as_deref()
        .filter(|_| config.uploads.shared_port())
        .and_then(crate::config::HostMatch::of)
    {
        Some(files_host) => shared_router(client, files_host),
        None => router(client),
    }
}

/// HOST1 single-port mode: a request whose `Host` names the user-content
/// origin goes to [`usercontent_router`] (only `/u/*`, its safety headers,
/// never a cookie); every other request to [`router`]. The two origins stay
/// distinct to the browser, so the separation of D50 holds on one port.
pub fn shared_router(client: Client, files_host: crate::config::HostMatch) -> Router {
    use tower::ServiceExt;
    let app = router(client.clone());
    let files = usercontent_router(client);
    let files_host = Arc::new(files_host);
    Router::new().fallback(move |request: Request<Body>| {
        let (app, files, files_host) = (app.clone(), files.clone(), files_host.clone());
        async move {
            let to_files = request
                .headers()
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .or_else(|| request.uri().authority().map(|a| a.as_str()))
                .is_some_and(|host| files_host.matches(host));
            let service = if to_files { files } else { app };
            match service.oneshot(request).await {
                Ok(response) => response,
                Err(never) => match never {},
            }
        }
    })
}

/// D50: the user-content origin. It serves only `/u/*`:
/// - `PUT /u/{exe}/{evd}?size&type&expires&sig`: the signed upload (D49),
///   with a CORS preflight for the app origin (`OPTIONS`);
/// - `GET /u/{exe}/{evd}[/thumbnail]?expires&sig`: a finalized file.
///
/// Everything else is 404. No cookie or credential is ever read, no cookie
/// is set, and every response carries `Content-Disposition`, `nosniff` and
/// `Content-Security-Policy: sandbox; default-src 'none'`.
pub fn usercontent_router(client: Client) -> Router {
    Router::new()
        .fallback(usercontent_dispatch)
        .with_state(client)
        .layer(axum::middleware::map_response(usercontent_headers))
}

/// The user-content safety headers on every response, errors included.
async fn usercontent_headers(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    headers.remove(header::SET_COOKIE);
    for (name, value) in runbook_api::evidence::USERCONTENT_HEADERS {
        headers.insert(
            http::HeaderName::from_static(name),
            http::HeaderValue::from_static(value),
        );
    }
    if !headers.contains_key(header::CONTENT_DISPOSITION) {
        headers.insert(
            header::CONTENT_DISPOSITION,
            http::HeaderValue::from_static("attachment"),
        );
    }
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(
            header::CACHE_CONTROL,
            http::HeaderValue::from_static("no-store"),
        );
    }
    response
}

async fn usercontent_dispatch(
    State(client): State<Client>,
    request: Request<Body>,
) -> Response<Body> {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .and_then(|now| crate::bootstrap::id("req_", now).ok())
        .unwrap_or_else(|| "req_unavailable".into());
    let path = request.uri().path().to_owned();
    // The app origin may PUT cross-origin (the web client); nothing else
    // gets CORS. Reads are plain `<img>`/link loads and need none.
    let cors = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|o| o.eq_ignore_ascii_case(&client.origin));
    let not_found = || encode(ApiResponse::error(404, "resource.not_found", &id));
    if !path.starts_with(runbook_core::uploads::signing::UPLOAD_PATH_PREFIX) {
        return not_found();
    }
    let method = request.method().clone();
    let mut response = match method {
        http::Method::OPTIONS => {
            let wants_put = request
                .headers()
                .get(header::ACCESS_CONTROL_REQUEST_METHOD)
                .is_some_and(|m| m == "PUT");
            if !cors || !wants_put {
                return not_found();
            }
            let mut response = Response::builder()
                .status(204)
                .header(header::ACCESS_CONTROL_ALLOW_METHODS, "PUT")
                .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "content-type")
                .header(header::ACCESS_CONTROL_MAX_AGE, "600")
                .body(Body::empty())
                .expect("fixed preflight");
            allow_origin(&mut response, &client.origin);
            return response;
        }
        http::Method::PUT => match tokio::time::timeout(
            client.upload_timeout,
            usercontent_put(&client, request, &id),
        )
        .await
        {
            Ok(response) => response,
            Err(_) => encode(ApiResponse::error(503, "server.timeout", &id)),
        },
        http::Method::GET | http::Method::HEAD => {
            usercontent_get(
                &client,
                &path,
                request.uri().query(),
                method == http::Method::HEAD,
                &id,
            )
            .await
        }
        _ => return not_found(),
    };
    if cors && method == http::Method::PUT {
        allow_origin(&mut response, &client.origin);
    }
    response
}

fn allow_origin(response: &mut Response<Body>, origin: &str) {
    if let Ok(value) = http::HeaderValue::from_str(origin) {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        response
            .headers_mut()
            .insert(header::VARY, http::HeaderValue::from_static("Origin"));
    }
}

fn wall_clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

fn problem(error: runbook_api::executions::ExecutionApiError, id: &str) -> Response<Body> {
    encode(ApiResponse::error(error.status, error.code, id))
}

/// D49 PUT, cheapest checks first (UP2 fixes): the URL, its signature and
/// its expiry by the wall clock (no database), then the headers against the
/// signed size and type, then the run's tenant and the slot's state (the
/// worker, store clock). Only then is a body budget reserved, from the
/// listener's own, and the body read, up to the signed size.
async fn usercontent_put(client: &Client, request: Request<Body>, id: &str) -> Response<Body> {
    use runbook_core::uploads::signing;
    let (parts, body) = request.into_parts();
    let Some(key) = client.upload_key.as_ref() else {
        return encode(ApiResponse::error(503, "uploads.not_configured", id));
    };
    let Some(query) = runbook_api::evidence::single_query(parts.uri.query()) else {
        return encode(ApiResponse::error(403, "upload.url_invalid", id));
    };
    let signed = match signing::parse_upload(parts.uri.path(), &query) {
        Ok(signed) => signed,
        Err(e) => return problem(runbook_api::evidence::url_error(e), id),
    };
    if let Err(e) = signing::verify(key, &signed.grant(), &signed.signature, wall_clock_ms()) {
        return problem(runbook_api::evidence::url_error(e), id);
    }
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let declared_type = content_type
        .as_deref()
        .map(|v| v.split(';').next().unwrap_or("").trim());
    if declared_type != Some(signed.content_type.as_str()) {
        return encode(ApiResponse::error(422, "upload.type_mismatch", id));
    }
    let length = match parts.headers.get(header::CONTENT_LENGTH) {
        None => None,
        Some(v) => match v.to_str().ok().and_then(|v| v.parse::<u64>().ok()) {
            Some(n) => Some(n),
            None => return encode(ApiResponse::error(400, "request.invalid_length", id)),
        },
    };
    if signed.size > client.max_request_bytes as u64 {
        return encode(ApiResponse::error(413, "request.body_too_large", id));
    }
    if length.is_some_and(|n| n != signed.size) {
        return encode(ApiResponse::error(422, "upload.size_mismatch", id));
    }
    let size = signed.size as usize;
    match files_call(
        client,
        runbook_api::evidence::UserContentRequest::PutCheck(signed.clone()),
    )
    .await
    {
        runbook_api::evidence::UserContentReply::Ready => {}
        other => return reply_response(client, other, false, id).await,
    }
    let reserve = size.div_ceil(1024).max(1);
    let Some(_budget) = u32::try_from(reserve)
        .ok()
        .and_then(|n| client.files_budget.clone().try_acquire_many_owned(n).ok())
    else {
        return encode(ApiResponse::error(503, "server.busy", id));
    };
    // One byte past the signed size is enough to know it does not match.
    let bytes = match to_bytes(body, size.saturating_add(1)).await {
        Ok(b) if b.len() == size => b,
        _ => return encode(ApiResponse::error(422, "upload.size_mismatch", id)),
    };
    let reply = files_call(
        client,
        runbook_api::evidence::UserContentRequest::Put {
            signed,
            content_type,
            bytes: bytes.to_vec(),
        },
    )
    .await;
    reply_response(client, reply, false, id).await
}

/// D50 GET/HEAD: the URL and its signature by the wall clock first, then the
/// worker resolves the object; the body streams in ranges.
async fn usercontent_get(
    client: &Client,
    path: &str,
    query: Option<&str>,
    head: bool,
    id: &str,
) -> Response<Body> {
    use runbook_core::uploads::signing;
    let Some(key) = client.upload_key.as_ref() else {
        return encode(ApiResponse::error(503, "uploads.not_configured", id));
    };
    let Some(query) = runbook_api::evidence::single_query(query) else {
        return encode(ApiResponse::error(403, "upload.url_invalid", id));
    };
    let signed = match signing::parse_read(path, &query) {
        Ok(signed) => signed,
        Err(e) => return problem(runbook_api::evidence::url_error(e), id),
    };
    if let Err(e) = signing::verify_read(key, &signed.grant(), &signed.signature, wall_clock_ms()) {
        return problem(runbook_api::evidence::url_error(e), id);
    }
    let reply = tokio::time::timeout(
        client.request_timeout,
        files_call(
            client,
            runbook_api::evidence::UserContentRequest::Get(signed),
        ),
    )
    .await
    .unwrap_or(runbook_api::evidence::UserContentReply::Problem {
        status: 503,
        code: "server.timeout",
    });
    reply_response(client, reply, head, id).await
}

async fn files_call(
    client: &Client,
    request: runbook_api::evidence::UserContentRequest,
) -> runbook_api::evidence::UserContentReply {
    use runbook_api::evidence::UserContentReply as Reply;
    let (reply, receive) = oneshot::channel();
    if client.files.try_send(FileCall { request, reply }).is_err() {
        return Reply::Problem {
            status: 503,
            code: "server.busy",
        };
    }
    receive.await.unwrap_or(Reply::Problem {
        status: 503,
        code: "server.unavailable",
    })
}

/// Bytes per range read while streaming a file.
const FILE_CHUNK_BYTES: u64 = 1_048_576;

async fn reply_response(
    client: &Client,
    reply: runbook_api::evidence::UserContentReply,
    head: bool,
    id: &str,
) -> Response<Body> {
    use runbook_api::evidence::{UserContentReply as Reply, UserContentRequest};
    match reply {
        Reply::Stored => encode(ApiResponse::json(204, serde_json::Value::Null, id)),
        Reply::Problem { status, code } => encode(ApiResponse::error(status, code, id)),
        Reply::Ready | Reply::Chunk(_) => encode(ApiResponse::error(500, "server.response", id)),
        Reply::File {
            content_type,
            name,
            inline,
            size,
            scope,
            key,
            max_age_s,
        } => {
            let content_type = runbook_api::evidence::served_content_type(&content_type);
            // Streamed in ranges, so a large file is never held in memory;
            // a failure mid-way ends the connection.
            let body = if head || size == 0 {
                Body::empty()
            } else {
                let client = client.clone();
                let timeout = client.request_timeout;
                Body::from_stream(futures_util::stream::unfold(0u64, move |at| {
                    let client = client.clone();
                    let (scope, key) = (scope.clone(), key.clone());
                    async move {
                        if at >= size {
                            return None;
                        }
                        let end = (at + FILE_CHUNK_BYTES).min(size);
                        let chunk = tokio::time::timeout(
                            timeout,
                            files_call(
                                &client,
                                UserContentRequest::Range {
                                    scope,
                                    key,
                                    range: at..end,
                                },
                            ),
                        )
                        .await;
                        Some(match chunk {
                            Ok(Reply::Chunk(bytes)) if bytes.len() as u64 == end - at => {
                                (Ok(axum::body::Bytes::from(bytes)), end)
                            }
                            _ => (Err(std::io::Error::other("evidence read failed")), size),
                        })
                    }
                }))
            };
            Response::builder()
                .status(200)
                .header("x-request-id", id)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CONTENT_LENGTH, size)
                .header(
                    header::CONTENT_DISPOSITION,
                    runbook_api::evidence::content_disposition(inline, &name),
                )
                .header(
                    header::CACHE_CONTROL,
                    format!("private, max-age={max_age_s}"),
                )
                .body(body)
                .unwrap_or_else(|_| encode(ApiResponse::error(500, "server.response", id)))
        }
    }
}

/// F45: accept at most `max_connections` sockets at once (the rest wait in
/// the listen backlog) and close a connection whose request headers do not
/// arrive within `header_read_timeout`. Runs until the process ends.
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    app: Router,
    limits: &crate::config::TransportLimits,
) -> Result<(), Error> {
    let (_keep, never) = watch::channel(false);
    serve_until(listener, app, limits, never).await
}

/// Resolves once the watched flag is true (holding no guard across awaits).
async fn stopped(flag: &mut watch::Receiver<bool>) {
    let _ = flag.wait_for(|s| *s).await;
}

/// [`serve_on`] until `stop` turns true (HOST1): then no connection is
/// accepted, each open one finishes its in-flight request and closes
/// (hyper's graceful shutdown), and this returns once all are closed. The
/// caller bounds the wait.
pub async fn serve_until(
    listener: tokio::net::TcpListener,
    app: Router,
    limits: &crate::config::TransportLimits,
    mut stop: watch::Receiver<bool>,
) -> Result<(), Error> {
    let max = limits.max_connections.max(1);
    let connections = Arc::new(tokio::sync::Semaphore::new(max));
    loop {
        let accepted = tokio::select! {
            () = stopped(&mut stop) => break,
            permit = connections.clone().acquire_owned() => {
                let permit = permit.map_err(|_| Error("server.serve"))?;
                tokio::select! {
                    () = stopped(&mut stop) => break,
                    accepted = listener.accept() => accepted.map(|a| (permit, a)),
                }
            }
        };
        let (permit, (stream, address)) = match accepted {
            Ok(accepted) => accepted,
            Err(_) => {
                // Out of descriptors or a reset handshake: back off, retry.
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        // AUTH2: the peer address reaches handlers as a request extension
        // (internal-route gate, per-IP limits, the proxy's x-runbook-client-ip).
        let service = hyper_util::service::TowerToHyperService::new(
            app.clone().layer(axum::Extension(PeerAddr(address))),
        );
        let header_read_timeout = limits.header_read_timeout;
        let mut stopping = stop.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(header_read_timeout);
            let connection =
                builder.serve_connection(hyper_util::rt::TokioIo::new(stream), service);
            tokio::pin!(connection);
            tokio::select! {
                _ = connection.as_mut() => {}
                () = stopped(&mut stopping) => {
                    connection.as_mut().graceful_shutdown();
                    let _ = connection.await;
                }
            }
        });
    }
    drop(listener);
    // Every connection task holds a permit until its connection closes.
    let _all = connections
        .acquire_many(u32::try_from(max).unwrap_or(u32::MAX))
        .await;
    Ok(())
}

async fn stream_response(
    client: Client,
    request: Request<Body>,
    nonce: String,
    id: &str,
) -> Response<Body> {
    if request.method() != http::Method::GET {
        return encode(ApiResponse::error(405, "request.method_not_allowed", id));
    }
    // CF4: the stream contract is shared with the Workers bridge.
    let after = match runbook_api::sse::cursor(
        request
            .headers()
            .get_all("last-event-id")
            .iter()
            .map(|v| v.to_str().ok()),
    ) {
        Ok(after) => after,
        Err(code) => return encode(ApiResponse::error(400, code, id)),
    };
    let (credential, cookie) = match credential(
        &client,
        request.method(),
        request.uri().path(),
        request.headers(),
        id,
    )
    .await
    {
        Ok(found) => found,
        Err(refused) => return encode(refused),
    };
    let r = runbook_api::Request {
        method: "GET".into(),
        path: request.uri().path().into(),
        query: Default::default(),
        body: serde_json::Value::Null,
        credential,
        idempotency_key: None,
        request_id: id.into(),
        new_id: nonce,
        if_match: None,
        client_ip: None,
    };
    let (reply, rx) = oneshot::channel();
    if client
        .streams
        .try_send(crate::event_stream::StreamCall {
            request: r,
            cookie,
            after,
            reply,
        })
        .is_err()
    {
        return encode(ApiResponse::error(503, "stream.capacity", id));
    }
    match tokio::time::timeout(client.request_timeout, rx).await {
        Ok(Ok(Ok(frames))) => {
            let stream = futures_util::stream::unfold(frames, |mut frames| async move {
                frames.recv().await.map(|frame| (frame, frames))
            });
            // HOST1: a stopping instance ends its streams; clients resume on
            // another with Last-Event-ID.
            let mut stopping = client.shutdown.clone();
            let stream =
                futures_util::StreamExt::take_until(
                    stream,
                    async move { stopped(&mut stopping).await },
                );
            let mut response = Response::builder().status(200);
            for (name, value) in runbook_api::sse::HEADERS {
                response = response.header(*name, *value);
            }
            response
                .header("x-request-id", id)
                .body(Body::from_stream(stream))
                .expect("fixed stream response")
        }
        Ok(Ok(Err(error))) => encode(error),
        _ => encode(ApiResponse::error(503, "stream.unavailable", id)),
    }
}
