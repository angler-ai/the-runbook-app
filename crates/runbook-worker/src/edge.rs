//! The Workers half (wasm32 only), CF3: the api Worker serves the real
//! `runbook_api::Router` over D1, R2 and the ExecutionDOs, and each
//! `ExecutionDO` runs the same router for its own run.
//!
//! - **api Worker** (`fetch`): authenticates, and for an execution-scoped
//!   request finds its run from verified facts only (§18): the execution
//!   bearer's route row (CF2-9), the credential's organization against the
//!   run's owner row, the step run's index row in that organization, or a
//!   new run's own nonce. It then forwards the request to that run's object;
//!   everything else it answers itself. `/v1/executions/{exe}/events` is
//!   authorized here, bridged to the object's WebSocket as SSE and
//!   re-authorized at every keep-alive (D32, D58).
//! - **ExecutionDO** (`/api…`): one request at a time per object (a gate
//!   held across the router's awaits), the router over backends that reach
//!   only this object, then the host settles (due work, derived rows into
//!   D1, admission notices), fans live frames out and publishes
//!   notifications to the queue (CF4). The alarm does the same.
//! - **Queue consumer and Cron**: webhook delivery and redelivery over D1
//!   and the AES-GCM sealer; admission notices for waiters promoted in D1.
//!
//! CF1/CF2's spike routes (start with a fault, state dump, outbox) exist
//! only on the object under the local test var `CF1_FAULT_INJECTION`, and
//! the api Worker has no route to them. LOCAL ONLY; nothing is deployed.
// Gate fix B1: the spike hooks and their imports exist in the test build
// only; the deploy build would warn about what only they use.
#![cfg_attr(not(feature = "test-seams"), allow(unused_imports, dead_code))]
use crate::{
    cloudflare::{CfBackends, CfHost, router_config},
    compose::Objects,
    sealer::AesSealer,
    settle::settle,
    spike::{self, SpikeError},
};
use runbook_api::{
    Credential,
    backends::{Backends, NoEntropy, encode_id},
    http::{Headers, decode, encode},
    problem::Response as ApiResponse,
    router::Router,
};
use runbook_core::{
    CommitResult, StoreError, WriteStage,
    catalog::TenantScope,
    credentials::{TokenKind, parse_secret, secret_hash},
    webhooks::TargetPolicy,
};
use runbook_model::{ExecutionId, OrganizationId};
use runbook_store_d1::workers::WorkersD1;
use runbook_store_do::{
    DoObject, DoSql, DoStore, Fault, NotificationCursor, OutboxStream, next_alarm_at,
    workers::WorkersSql,
};
use runbook_store_r2::workers::R2Bucket;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    task::{Poll, Waker},
};
use worker::{
    DurableObject, Env, Method, Request, Response, Result, SqlStorageValue, State, durable_object,
    event,
};

/// The largest request body the Worker reads, on every route: native's JSON
/// cap (CF12). It was 1 MiB, which refused a runbook source near the
/// compiler's 1 MiB ceiling; Workers Paid's CPU time has room for native's
/// cap. Skill uploads, which native allows up to `RUNBOOK_MAX_REQUEST_BYTES`
/// (64 MiB), keep this cap here: their JSON, its decoded files and the
/// request's own copy share the isolate's 128 MB.
const MAX_BODY_BYTES: u64 = runbook_api::http::JSON_BODY_BYTES;
/// How long the api Worker waits for an object (a trapped object never
/// answers, CF1).
const OBJECT_TIMEOUT_MS: u64 = 25_000;
/// Headers only the api Worker may set on a forwarded request.
const NEW_ID: &str = "x-runbook-new-id";
const REQUEST_ID: &str = "x-runbook-request-id";
/// Set on every response an ExecutionDO's router produced.
const OBJECT_HEADER: &str = "x-runbook-object";

#[path = "operator.rs"]
mod operator;
#[path = "usercontent.rs"]
mod usercontent;
#[path = "web.rs"]
mod web;

fn problem(status: u16, code: &str) -> Result<Response> {
    Ok(Response::from_json(&json!({ "error": code }))?.with_status(status))
}

// ---------------------------------------------------------------------------
// The platform.

/// The object protocol over Durable Object stubs (CF5): one `POST
/// https://do/{exe}/op` per call to the run's ExecutionDO, which only
/// Worker code can address (the api Worker forwards clients under
/// `/{exe}/api/` only).
#[derive(Clone)]
pub struct DoTransport {
    namespace: worker::ObjectNamespace,
    /// CF6: one OrgDO per organization.
    orgs: worker::ObjectNamespace,
    /// CF11: one WebhookDO per webhook subscription.
    webhooks: worker::ObjectNamespace,
}
impl DoTransport {
    pub(crate) fn new(env: &Env) -> Result<Self> {
        Ok(Self {
            namespace: env.durable_object("EXECUTIONS")?,
            orgs: env.durable_object("ORGS")?,
            webhooks: env.durable_object("WEBHOOKS")?,
        })
    }
}

/// POST `body` to a Durable Object stub and read the protocol reply.
async fn post_reply(stub: worker::Stub, url: &str, body: String) -> crate::protocol::Reply {
    let failed = |_| StoreError::Backend("object.unreachable");
    let mut init = worker::RequestInit::new();
    init.with_method(Method::Post)
        .with_body(Some(wasm_bindgen::JsValue::from_str(&body)));
    let outgoing = Request::new_with_init(url, &init).map_err(failed)?;
    let mut response = match futures_util::future::select(
        Box::pin(stub.fetch_with_request(outgoing)),
        Box::pin(worker::Delay::from(std::time::Duration::from_millis(
            OBJECT_TIMEOUT_MS,
        ))),
    )
    .await
    {
        futures_util::future::Either::Left((Ok(response), _)) => response,
        futures_util::future::Either::Left((Err(_), _)) => {
            return Err(StoreError::Backend("object.unreachable"));
        }
        futures_util::future::Either::Right(_) => {
            return Err(StoreError::Backend("object.timeout"));
        }
    };
    let text = response.text().await.map_err(failed)?;
    crate::protocol::decode_reply(
        serde_json::from_str(&text).map_err(|_| StoreError::Backend("object.bad_reply"))?,
    )
}
impl crate::protocol::Transport for DoTransport {
    async fn call(&self, request: crate::protocol::Request) -> crate::protocol::Reply {
        let failed = |_| StoreError::Backend("object.unreachable");
        let id = request.execution_id.clone();
        let stub = self
            .namespace
            .id_from_name(id.as_str())
            .map_err(failed)?
            .get_stub()
            .map_err(failed)?;
        let body =
            serde_json::to_string(&request).map_err(|_| StoreError::Backend("object.encode"))?;
        post_reply(stub, &format!("https://do/{id}/op"), body).await
    }
    async fn call_org(&self, request: crate::quota::QuotaRequest) -> crate::protocol::Reply {
        let failed = |_| StoreError::Backend("object.unreachable");
        let stub = self
            .orgs
            .id_from_name(request.org.as_str())
            .map_err(failed)?
            .get_stub()
            .map_err(failed)?;
        let body =
            serde_json::to_string(&request).map_err(|_| StoreError::Backend("quota.encode"))?;
        post_reply(stub, "https://org/quota", body).await
    }
    async fn call_webhook(
        &self,
        request: crate::webhooks::WebhookRequest,
    ) -> crate::protocol::Reply {
        let failed = |_| StoreError::Backend("object.unreachable");
        let stub = self
            .webhooks
            .id_from_name(request.subscription.as_str())
            .map_err(failed)?
            .get_stub()
            .map_err(failed)?;
        let body =
            serde_json::to_string(&request).map_err(|_| StoreError::Backend("webhooks.encode"))?;
        post_reply(stub, "https://webhook/op", body).await
    }
}

/// Which ExecutionDO storage a host reaches (CF5): an ExecutionDO its own
/// run's locally and every other run's over the object protocol; the api
/// Worker every run's over the protocol. Only the object's own run is
/// listed (`reachable`): the api Worker reaches objects by ID only.
#[derive(Clone)]
pub struct WorkersObjects {
    own: Option<(Rc<DoObject<WorkersSql>>, ExecutionId)>,
    remote: DoTransport,
}
impl WorkersObjects {
    /// The api Worker's: every run over the protocol.
    fn remote(env: &Env) -> Result<Self> {
        Ok(Self {
            own: None,
            remote: DoTransport::new(env)?,
        })
    }
    /// An ExecutionDO's for run `id`: its own storage, others remote.
    fn own(env: &Env, object: Rc<DoObject<WorkersSql>>, id: &ExecutionId) -> Result<Self> {
        Ok(Self {
            own: Some((object, id.clone())),
            remote: DoTransport::new(env)?,
        })
    }
}
impl Objects for WorkersObjects {
    type Handle = crate::protocol::Hybrid<WorkersSql, DoTransport>;
    type Quota = crate::quota::RemoteQuota<DoTransport>;
    fn quota(&self, org: &runbook_model::OrganizationId) -> Self::Quota {
        crate::quota::RemoteQuota::new(self.remote.clone(), org.clone())
    }
    type Webhooks = crate::webhooks::ViaTransport<DoTransport>;
    fn webhooks(&self) -> Self::Webhooks {
        crate::webhooks::ViaTransport(self.remote.clone())
    }
    fn object(
        &self,
        id: &ExecutionId,
        scope: &TenantScope,
    ) -> std::result::Result<Self::Handle, StoreError> {
        match &self.own {
            Some((object, own)) if own == id => object
                .store(id, scope.clone())
                .map(crate::protocol::Hybrid::Local)
                .map_err(|error| match error {
                    StoreError::Backend("do.object_mismatch") => StoreError::NotFound,
                    other => other,
                }),
            _ => Ok(crate::protocol::Hybrid::Remote(
                crate::protocol::RemoteObject::new(self.remote.clone(), id.clone(), scope.clone()),
            )),
        }
    }
    fn reachable(&self) -> Vec<ExecutionId> {
        match &self.own {
            None => vec![],
            Some((object, _)) => object.stored_name().ok().flatten().into_iter().collect(),
        }
    }
}

#[derive(Clone)]
pub struct WorkersWebhookHost {
    sealer: Option<Rc<AesSealer>>,
    policy: TargetPolicy,
}
impl runbook_api::webhooks::WebhookHost for WorkersWebhookHost {
    type Sealer = AesSealer;
    fn sealer(&self) -> Option<&AesSealer> {
        self.sealer.as_deref()
    }
    fn random32(&self) -> Option<[u8; 32]> {
        let mut bytes = [0u8; 32];
        crate::notify::crypto_fill(&mut bytes).ok().map(|()| bytes)
    }
    fn target_policy(&self) -> TargetPolicy {
        self.policy
    }
    /// Workers cannot resolve a name: the URL's syntax and a literal
    /// address are vetted (CF4 "Transport"); egress cannot reach private
    /// networks, which is the remaining guard.
    async fn vet_target(&self, url: &str) -> std::result::Result<(), &'static str> {
        crate::notify::vet(url, self.policy)
    }
}

#[derive(Clone)]
pub struct WorkersHost {
    /// CF11: the D1 databases by the shard map (`RUNBOOK_D1_SHARDS`).
    shards: crate::shards::D1Shards<WorkersD1>,
    blobs: R2Bucket,
    user_content: R2Bucket,
    objects: WorkersObjects,
    webhooks: WorkersWebhookHost,
    runner_mtls: Option<worker::Fetcher>,
    retention: runbook_core::archives::job::RetentionPolicy,
    retention_offset_ms: i64,
    quota: runbook_core::quotas::QuotaLimits,
    /// CF13 review round 1: the run budget.
    run_budget: u64,
    /// The per-run step cap (`RUNBOOK_LIMIT_STEP_RUNS`).
    step_cap: Option<u64>,
    /// SLACK1 stage 2: the operator's Slack app.
    slack: Option<runbook_core::slack::SlackApp>,
    /// CF7: the sign-in methods discovery lists (the auth Worker's).
    auth_methods: Vec<String>,
    /// MCP2: the auth Worker, for the connected-app routes.
    auth: Option<crate::auth::WorkersSessions>,
    /// Gate fix B2: the rate-limit windows (one LimitsDO per key).
    limits: DoLimits,
    /// NOTIFY1: the app's public origin, for links in email.
    public_origin: String,
}
impl WorkersHost {
    fn new(env: &Env, objects: WorkersObjects) -> Result<Self> {
        Ok(Self {
            shards: env_shards(env)?,
            blobs: R2Bucket(env.bucket("BLOBS")?),
            user_content: R2Bucket(env.bucket("USER_CONTENT")?),
            objects,
            runner_mtls: env.service("RUNBOOK_RUNNER_MTLS").ok(),
            webhooks: WorkersWebhookHost {
                sealer: crate::notify::sealer(env).map(Rc::new),
                policy: crate::notify::policy(env),
            },
            // D54 from the Worker's vars; an invalid setting keeps the
            // defaults and says so.
            retention: crate::archives::retention_policy(|name| {
                env.var(name).ok().map(|v| v.to_string())
            })
            .unwrap_or_else(|code| {
                worker::console_log!("retention vars ignored: {code}");
                crate::archives::retention_policy(|_| None).unwrap_or_default()
            }),
            // A local test seam (the retention smoke's accelerated clock).
            retention_offset_ms: crate::seams::var(env, "RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0),
            auth_methods: vec![],
            auth: crate::auth::WorkersSessions::from_env(env, &origin(env)),
            limits: DoLimits::new(env),
            public_origin: origin(env),
            // CF13 review round 1: the default unless set; an invalid value
            // keeps the default and says so.
            run_budget: runbook_core::run_budget::parse_run_budget(
                env.var("RUNBOOK_LIMIT_RUN_BYTES")
                    .ok()
                    .map(|v| v.to_string())
                    .as_deref(),
            )
            .unwrap_or_else(|code| {
                worker::console_log!("run budget var ignored: {code}");
                runbook_core::run_budget::DEFAULT_MAX_RUN_BYTES
            }),
            step_cap: step_cap(env),
            slack: slack_app(env),
            // CF6: off unless set; an invalid value keeps it off and says so.
            quota: runbook_core::quotas::QuotaLimits {
                executions_monthly: runbook_core::quotas::parse_limit(
                    env.var("RUNBOOK_LIMIT_EXECUTIONS_MONTHLY")
                        .ok()
                        .map(|v| v.to_string())
                        .as_deref(),
                )
                .unwrap_or_else(|code| {
                    worker::console_log!("quota vars ignored: {code}");
                    None
                }),
            },
        })
    }
}
impl CfHost for WorkersHost {
    type Sql = WorkersSql;
    type D1 = WorkersD1;
    type Bucket = R2Bucket;
    type Objects = WorkersObjects;
    type WebhookHost = WorkersWebhookHost;
    type Limits = DoLimits;
    fn limits(&self) -> DoLimits {
        self.limits.clone()
    }
    fn public_origin(&self) -> String {
        self.public_origin.clone()
    }
    fn shards(&self) -> crate::shards::D1Shards<WorkersD1> {
        self.shards.clone()
    }
    fn buckets(&self) -> (R2Bucket, R2Bucket) {
        (self.blobs.clone(), self.user_content.clone())
    }
    fn objects(&self) -> WorkersObjects {
        self.objects.clone()
    }
    fn webhook_host(&self) -> WorkersWebhookHost {
        self.webhooks.clone()
    }
    async fn runner_fetch(
        &self,
        request: runbook_api::runner_proxy::RunnerHttpRequest,
    ) -> std::result::Result<runbook_api::runner_proxy::RunnerHttpResponse, &'static str> {
        crate::runner_proxy::fetch(self.runner_mtls.as_ref(), request).await
    }
    fn now_ms(&self) -> i64 {
        #[allow(clippy::cast_possible_truncation)]
        let now = js_sys::Date::now() as i64;
        now
    }
    fn fill_random(&self, bytes: &mut [u8]) -> std::result::Result<(), NoEntropy> {
        crate::notify::crypto_fill(bytes).map_err(|()| NoEntropy)
    }
    fn retention(&self) -> runbook_core::archives::job::RetentionPolicy {
        self.retention
    }
    fn retention_clock_offset_ms(&self) -> i64 {
        self.retention_offset_ms
    }
    fn quota_limits(&self) -> runbook_core::quotas::QuotaLimits {
        self.quota
    }
    fn run_budget(&self) -> u64 {
        self.run_budget
    }
    fn step_cap(&self) -> Option<u64> {
        self.step_cap
    }
    fn slack_app(&self) -> Option<runbook_core::slack::SlackApp> {
        self.slack.clone()
    }
    async fn slack_api(
        &self,
        token: &str,
        method: &str,
        query: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> std::result::Result<serde_json::Value, String> {
        use runbook_core::slack::SlackTransport;
        crate::notify::SlackFetch
            .slack_api(token, method, query, body)
            .await
    }
    async fn slack_respond(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        crate::notify::SlackFetch.respond(url, body).await
    }
    fn auth_methods(&self) -> Vec<String> {
        self.auth_methods.clone()
    }
    async fn auth_internal(
        &self,
        route: &'static str,
        body: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, runbook_api::session::Unavailable> {
        match &self.auth {
            Some(sessions) => sessions.auth_internal(route, &body).await,
            None => Ok(None),
        }
    }
}

thread_local! {
    /// The D1 schema was found current in this isolate.
    static MIGRATED: Cell<bool> = const { Cell::new(false) };
    /// UI-1: the theme's error or contrast warnings were logged in this
    /// isolate.
    static THEME_CHECKED: Cell<bool> = const { Cell::new(false) };
}
/// A plain-text var by name (`None` when unbound).
fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string())
}
/// UI-1: the theme override from the Worker's `RUNBOOK_THEME_*` vars, read
/// as natively ([`runbook_api::discovery::theme_from_vars`]).
fn theme(
    env: &Env,
) -> std::result::Result<Option<runbook_model::ThemeView>, runbook_api::discovery::ThemeError> {
    runbook_api::discovery::theme_from_vars(|name| var(env, name))
}
/// Bring D1's schema up to date once per isolate (local: the first
/// request; deployed instances apply migrations before traffic, CF10).
async fn ensure_schema(env: &Env) -> Result<()> {
    if MIGRATED.with(Cell::get) {
        return Ok(());
    }
    // Every database the shard map names has the whole schema (CF11).
    for db in env_shards(env)?.every() {
        runbook_store_d1::migrate::migrate(&db)
            .await
            .map_err(|e| worker::Error::from(format!("d1.migrate: {e:?}")))?;
    }
    MIGRATED.with(|m| m.set(true));
    Ok(())
}

/// The router's configuration from the Worker's bindings (CF7): sign-in
/// when the auth Worker is bound (with its registration domains,
/// `RUNBOOK_AUTH_ALLOWED_DOMAINS`), uploads with a signing key and a
/// separate user-content origin (`usercontent::settings`).
/// SLACK1 stage 2: the Slack app from the Worker's secrets (or vars).
pub(crate) fn slack_app(env: &Env) -> Option<runbook_core::slack::SlackApp> {
    runbook_core::slack::SlackApp::from_vars(|name| {
        env.secret(name)
            .ok()
            .map(|v| v.to_string())
            .or_else(|| var(env, name))
    })
}
/// The per-run step cap (`RUNBOOK_LIMIT_STEP_RUNS`): none unless set; an
/// invalid value keeps none and says so.
fn step_cap(env: &Env) -> Option<u64> {
    runbook_core::run_budget::parse_step_cap(
        env.var("RUNBOOK_LIMIT_STEP_RUNS")
            .ok()
            .map(|v| v.to_string())
            .as_deref(),
    )
    .unwrap_or_else(|code| {
        worker::console_log!("step cap var ignored: {code}");
        None
    })
}

fn worker_router_config(env: &Env) -> runbook_api::router::RouterConfig {
    let origin = origin(env);
    let mut config = router_config(&origin, MAX_BODY_BYTES);
    config.discovery.step_runs = step_cap(env);
    // UPD1 test seam: the update tests run one build as two releases.
    if let Some(release) = crate::seams::var(env, "RUNBOOK_TEST_RELEASE") {
        config.discovery.version = release;
    }
    if crate::auth::WorkersSessions::from_env(env, &origin).is_some() {
        config.sign_in = Some(runbook_api::router::SignIn {
            allowed_domains: env
                .var("RUNBOOK_AUTH_ALLOWED_DOMAINS")
                .map(|v| v.to_string())
                .unwrap_or_default()
                .split(',')
                .map(|d| d.trim().trim_start_matches('@').to_ascii_lowercase())
                .filter(|d| !d.is_empty())
                .collect(),
        });
    }
    config.uploads = usercontent::settings(env, &origin);
    config.upload_limits = config.uploads.limits.clone();
    // An invalid theme is ignored as a whole (`fetch` logs it).
    config.discovery.theme = theme(env).ok().flatten();
    config
}

pub(crate) fn origin(env: &Env) -> String {
    env.var("RUNBOOK_ORIGIN")
        .or_else(|_| env.var("ORIGIN"))
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "http://localhost:8787".into())
}

fn nonce<H: CfHost>(host: &H) -> std::result::Result<String, NoEntropy> {
    let mut random = [0u8; 10];
    host.fill_random(&mut random)?;
    encode_id("", host.now_ms(), random).ok_or(NoEntropy)
}

// ---------------------------------------------------------------------------
// HTTP conversion.

/// The parts of an incoming request the router and a forward need.
struct Incoming {
    method: String,
    path: String,
    query: Option<String>,
    headers: Headers,
    raw_headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// The body is over [`MAX_BODY_BYTES`] (by its declared length, then
    /// not read, or as read).
    too_large: bool,
}
async fn read_incoming(req: &mut Request, path: String, query: Option<String>) -> Result<Incoming> {
    let mut headers = Headers::new();
    let mut raw_headers = vec![];
    for (name, value) in req.headers().entries() {
        headers.push(&name, value.as_bytes().to_vec());
        raw_headers.push((name, value));
    }
    // As natively (F45): a declared length over the cap is refused before
    // any body byte is read; CF12 review round 1: a chunked body is read
    // with a running cap and refused once past it.
    let (body, too_large) = match crate::body::read(req, MAX_BODY_BYTES).await? {
        crate::body::Capped::Body(body) => (body, false),
        crate::body::Capped::TooLarge => (vec![], true),
    };
    Ok(Incoming {
        method: req.method().to_string(),
        path,
        query,
        headers,
        raw_headers,
        body,
        too_large,
    })
}
fn to_request(
    incoming: &Incoming,
    request_id: &str,
    new_id: String,
    credential: Option<Credential>,
) -> std::result::Result<runbook_api::Request, ApiResponse> {
    if incoming.too_large {
        return Err(ApiResponse::error(
            413,
            "request.body_too_large",
            request_id,
        ));
    }
    runbook_api::http::plan_body(
        &incoming.method,
        &incoming.path,
        &incoming.headers,
        request_id,
    )?;
    let mut request = decode(
        &incoming.method,
        &incoming.path,
        incoming.query.as_deref(),
        &incoming.headers,
        &incoming.body,
        credential,
        request_id,
        new_id,
    )?;
    // Sign-in gate fix 3: device codes record who asked; Cloudflare sets
    // this header itself (a client's own value never survives the edge).
    request.client_ip = incoming
        .headers
        .get("cf-connecting-ip")
        .and_then(|ip| ip.trim().parse::<std::net::IpAddr>().ok())
        .map(|ip| ip.to_string());
    Ok(request)
}
/// D46: how a request authenticates: a bearer, or (CF7, with sign-in
/// configured) a session cookie the auth Worker vouches for, after CSRF.
async fn credential(
    env: &Env,
    incoming: &Incoming,
    request_id: &str,
) -> std::result::Result<Option<Credential>, ApiResponse> {
    let sessions = crate::auth::WorkersSessions::from_env(env, &origin(env));
    runbook_api::session::credential(
        sessions.as_ref(),
        &incoming.method,
        &incoming.path,
        &incoming.headers,
        request_id,
    )
    .await
    .map(|(credential, _)| credential)
}

fn to_response(response: ApiResponse) -> Result<Response> {
    http_response(encode(response))
}
fn http_response(encoded: runbook_api::http::HttpResponse) -> Result<Response> {
    let mut out = Response::from_bytes(encoded.body)?.with_status(encoded.status);
    let headers = out.headers_mut();
    // `from_bytes` sets `application/octet-stream`; the encoded response's
    // own type (if any) is the only one (CF7 fix: it was appended).
    headers.delete("content-type")?;
    for (name, value) in &encoded.headers {
        headers.append(name, value)?;
    }
    headers.set("x-content-type-options", "nosniff")?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// The api Worker.

/// Which run's object answers this request, from verified facts only
/// (§18). `None`: the api Worker answers it (the router then finds no
/// object here and refuses what it cannot serve).
async fn target(
    backends: &CfBackends<WorkersHost>,
    request: &runbook_api::Request,
) -> std::result::Result<Option<ExecutionId>, StoreError> {
    use runbook_api::directory::match_path;
    // An execution or refresh secret routes by its hash; the object
    // rechecks the token itself.
    if let Some(secret) = request.bearer()
        && matches!(
            parse_secret(secret),
            Ok((TokenKind::Execution | TokenKind::Refresh, _))
        )
    {
        return backends
            .credentials()
            .execution_route(&secret_hash(secret))
            .await
            .map(|route| route.map(|(_, exe)| exe))
            .map_err(|_| StoreError::Backend("d1.credentials"));
    }
    let path = request.path.as_str();
    // The refresh body authenticates (PRD §9): route by the refresh
    // token's own route row, and only to the run the path names. An
    // unknown run and a bad token both stay here and get the router's one
    // answer, so the route never tells whether a run ID exists (review 7).
    if let Some(p) = match_path("/v1/executions/{exe}/tokens/refresh", path) {
        let Some(token) = request.body.get("refresh_token").and_then(|t| t.as_str()) else {
            return Ok(None);
        };
        let routed = backends
            .credentials()
            .execution_route(&secret_hash(token))
            .await
            .map_err(|_| StoreError::Backend("d1.credentials"))?;
        return Ok(routed
            .map(|(_, exe)| exe)
            .filter(|exe| exe.as_str() == p["exe"]));
    }
    let step_param = [
        "/v1/step-runs/{sr}",
        "/v1/step-runs/{sr}/{rest}",
        "/v1/step-runs/{sr}/{rest}/{more}",
        "/v1/step-runs/{sr}/{rest}/{more}/{last}",
    ]
    .iter()
    .find_map(|pattern| match_path(pattern, path))
    .and_then(|p| p.get("sr").cloned());

    if let Some(sr) = &step_param
        && runbook_api::runner_access::is_runner(request)
        && let Some(secret) = request.bearer()
    {
        use runbook_core::runners::{RunnerError, RunnerStore, token_hash};
        // The registry provides the tenant for object routing. The
        // object's router rechecks the token, route and live claim.
        let principal = backends
            .runners()
            .authenticate(&token_hash(secret))
            .await
            .map_err(|error| match error {
                RunnerError::Store(error) => error,
                _ => StoreError::Backend("d1.runner_registry"),
            })?;
        let Some(principal) = principal else {
            return Ok(None);
        };
        if runbook_api::runner_access::allowed(principal.purpose, &request.method, path).is_none() {
            return Ok(None);
        }
        return step_execution(backends, &principal.org_id, sr).await;
    }
    // CF7: a session routes by the user it links to (D45), as the router
    // will link it.
    let record = match &request.credential {
        Some(Credential::Session(session)) => {
            let mut session = session.clone();
            if runbook_api::session::link(backends, &mut session)
                .await
                .is_err()
            {
                return Ok(None);
            }
            match session.principal.as_deref() {
                Some(record) => record.clone(),
                None => return Ok(None),
            }
        }
        _ => match request.credential_record(&backends.credentials()).await {
            Ok(record) => record,
            Err(_) => return Ok(None),
        },
    };
    let org = record.org_id.clone();
    if match_path("/v1/executions/{exe}/events", path).is_some()
        || match_path("/v1/executions/{exe}/archive", path).is_some()
    {
        return Ok(None);
    }
    let exe_param = [
        "/v1/executions/{exe}",
        "/v1/executions/{exe}/{rest}",
        "/v1/executions/{exe}/{rest}/{more}",
    ]
    .iter()
    .find_map(|pattern| match_path(pattern, path))
    .and_then(|p| p.get("exe").cloned());
    if let Some(exe) = exe_param {
        let Ok(exe) = exe.parse::<ExecutionId>() else {
            return Ok(None);
        };
        // The run's owner must be the credential's organization.
        return Ok(backends
            .execution_owner(&org, &exe)
            .await?
            .filter(|owner| owner.org_id == org)
            .map(|_| exe));
    }
    if let Some(sr) = step_param {
        return step_execution(backends, &org, &sr).await;
    }
    // LANG9b: a webhook trigger's call may start a run, so it runs in the
    // object of the run it would create; the router there answers a
    // duplicate from the run it already started.
    if request.method == "POST" && match_path(runbook_model::TRIGGER_PATH, path).is_some() {
        return Ok(format!("exe_{}", request.new_id).parse().ok());
    }
    if request.method == "POST"
        && let Some(p) = match_path("/v1/projects/{p}/executions", path)
    {
        // Only for a project of the credential's organization; the object's
        // router authorizes the rest. A retried key answers from its run.
        let directory = backends.directory();
        let snapshot = runbook_core::directory::DirectoryStore::snapshot(&directory, &org)
            .await
            .map_err(|_| StoreError::Backend("d1.directory"))?;
        let Some(project) = snapshot
            .projects
            .values()
            .find(|project| project.id.as_str() == p["p"] || project.slug == p["p"])
        else {
            return Ok(None);
        };
        if let Some(key) = &request.idempotency_key {
            // The saved start's key, framed as the router frames it.
            use runbook_core::credentials::{ActorKind, CredentialPrincipal};
            let principal = match &record.principal {
                CredentialPrincipal::User {
                    user_id,
                    actor: ActorKind::Human,
                } => format!("human:{user_id}"),
                CredentialPrincipal::User { user_id, .. } => format!("agent:{user_id}"),
                CredentialPrincipal::Service { .. } => format!("service:{}", record.id),
            };
            let framed = serde_json::to_vec(&json!(["execution.start", principal, key]))
                .map_err(|_| StoreError::Backend("start.key"))?;
            let key =
                &runbook_core::authoring::hex(&<sha2::Sha256 as sha2::Digest>::digest(framed));
            let scope = TenantScope {
                org_id: org.clone(),
                project_id: project.id.clone(),
            };
            if let Some(replay) = backends
                .execution(scope)
                .catalog()
                .find_start_replay(key)
                .await?
            {
                return Ok(Some(replay.outcome.execution_id));
            }
        }
        return Ok(format!("exe_{}", request.new_id).parse().ok());
    }
    Ok(None)
}

/// Send the request to `exe`'s object and relay its answer.
async fn forward(
    env: &Env,
    exe: &ExecutionId,
    incoming: &Incoming,
    new_id: &str,
    request_id: &str,
) -> Result<Response> {
    let stub = env
        .durable_object("EXECUTIONS")?
        .id_from_name(exe.as_str())?
        .get_stub()?;
    let query = incoming
        .query
        .as_deref()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let mut init = worker::RequestInit::new();
    init.with_method(Method::from(incoming.method.clone()));
    let headers = worker::Headers::new();
    for (name, value) in &incoming.raw_headers {
        // Only what the api Worker alone may set is dropped (a client's
        // `x-runbook-csrf`, D46, goes through: the object checks it too).
        let lower = name.to_ascii_lowercase();
        if ![
            NEW_ID,
            REQUEST_ID,
            OBJECT_HEADER,
            crate::auth::INTERNAL_SECRET_HEADER,
        ]
        .contains(&lower.as_str())
            && lower != "x-runbook-client-ip"
        {
            headers.append(name, value)?;
        }
    }
    headers.set(NEW_ID, new_id)?;
    headers.set(REQUEST_ID, request_id)?;
    init.with_headers(headers);
    if !incoming.body.is_empty() {
        init.with_body(Some(
            js_sys::Uint8Array::from(incoming.body.as_slice()).into(),
        ));
    }
    let request = Request::new_with_init(
        &format!("https://do/{exe}/api{}{query}", incoming.path),
        &init,
    )?;
    let call = stub.fetch_with_request(request);
    match futures_util::future::select(
        Box::pin(call),
        Box::pin(worker::Delay::from(std::time::Duration::from_millis(
            OBJECT_TIMEOUT_MS,
        ))),
    )
    .await
    {
        futures_util::future::Either::Left((response, _)) => response,
        futures_util::future::Either::Right(_) => {
            to_response(ApiResponse::error(503, "execution.unavailable", request_id))
        }
    }
}

async fn step_execution(
    backends: &CfBackends<WorkersHost>,
    org: &OrganizationId,
    step: &str,
) -> std::result::Result<Option<ExecutionId>, StoreError> {
    let Ok(step) = step.parse() else {
        return Ok(None);
    };
    backends
        .execution(TenantScope {
            org_id: org.clone(),
            project_id: runbook_api::backends::zero_project(),
        })
        .catalog()
        .step_run_execution(org, &step)
        .await
        .map(|result| result.map(|(execution, _)| execution))
}

/// The API, except that a person who opens a refused `/v1` address in the
/// browser (say, from an email) gets the sign-in or not-found page rather
/// than the problem JSON, as natively.
async fn api(req: Request, env: Env, path: String, query: Option<String>) -> Result<Response> {
    let header = |name: &str| req.headers().get(name).ok().flatten();
    let method = if req.method() == Method::Get {
        "GET"
    } else {
        "OTHER"
    };
    let navigation = runbook_api::web::is_navigation(
        method,
        header("sec-fetch-dest").as_deref(),
        header("accept").as_deref(),
    );
    let callback_failure =
        || runbook_api::http::oauth_callback_failure(method, &path, "req_callback");
    let response = match api_inner(req, env, path.clone(), query).await {
        Ok(response) => response,
        Err(error) => return callback_failure().map_or(Err(error), to_response),
    };
    let problem = response
        .headers()
        .get("content-type")
        .ok()
        .flatten()
        .is_some_and(|v| v.starts_with("application/problem+json"));
    if problem && let Some(failed) = callback_failure() {
        return to_response(failed);
    }
    if navigation
        && problem
        && let Some(location) = runbook_api::web::navigation_redirect(&path, response.status_code())
    {
        let mut redirect = Response::empty()?.with_status(302);
        redirect.headers_mut().set("location", location)?;
        redirect.headers_mut().set("cache-control", "no-store")?;
        return Ok(redirect);
    }
    Ok(response)
}

async fn api_inner(
    mut req: Request,
    env: Env,
    path: String,
    query: Option<String>,
) -> Result<Response> {
    ensure_schema(&env).await?;
    let mut host = WorkersHost::new(&env, WorkersObjects::remote(&env)?)?;
    host.auth_methods = auth_methods(&env).await;
    let backends = CfBackends::new(host.clone());
    let (Ok(new_id), Ok(request_tail)) = (nonce(&host), nonce(&host)) else {
        return problem(503, "server.entropy");
    };
    let request_id = format!("req_{request_tail}");
    let incoming = read_incoming(&mut req, path, query).await?;
    let credential = match credential(&env, &incoming, &request_id).await {
        Ok(credential) => credential,
        Err(response) => return to_response(response),
    };
    let request = match to_request(&incoming, &request_id, new_id.clone(), credential) {
        Ok(request) => request,
        Err(response) => return to_response(response),
    };
    let start = incoming.method == "POST"
        && (runbook_api::directory::match_path("/v1/projects/{p}/executions", &incoming.path)
            .is_some()
            || runbook_api::directory::match_path(runbook_model::TRIGGER_PATH, &incoming.path)
                .is_some());
    match target(&backends, &request).await {
        Ok(Some(exe)) if start => {
            // A start runs in the object of the run it creates or finishes:
            // the router names the run `exe_{new_id}`.
            let named = exe
                .as_str()
                .strip_prefix("exe_")
                .unwrap_or(&new_id)
                .to_owned();
            let response = forward(&env, &exe, &incoming, &named, &request_id).await?;
            if response.status_code() != 409 {
                return Ok(response);
            }
            // Review fix 1: a concurrent start under the same key reserved
            // another run first; send this request to that run once.
            let status = response.status_code();
            let headers = response.headers().clone();
            let mut response = response;
            let body = response.bytes().await.unwrap_or_default();
            if !String::from_utf8_lossy(&body).contains(crate::compose::REPLAYED_ELSEWHERE) {
                let mut out = Response::from_bytes(body)?.with_status(status);
                for (name, value) in headers.entries() {
                    out.headers_mut().set(&name, &value)?;
                }
                return Ok(out);
            }
            match target(&backends, &request).await {
                Ok(Some(saved)) if saved != exe => {
                    let named = saved
                        .as_str()
                        .strip_prefix("exe_")
                        .unwrap_or(&new_id)
                        .to_owned();
                    forward(&env, &saved, &incoming, &named, &request_id).await
                }
                _ => to_response(ApiResponse::error(503, "start.unavailable", &request_id)),
            }
        }
        Ok(Some(exe)) => forward(&env, &exe, &incoming, &new_id, &request_id).await,
        Ok(None) => {
            let router = Router::new(backends, worker_router_config(&env));
            to_response(router.handle(request).await)
        }
        Err(_) => to_response(ApiResponse::error(503, "storage.unavailable", &request_id)),
    }
}

/// D58: `GET /v1/executions/{exe}/events` as SSE, bridged from the run's
/// object. The caller is authorized first (native `authorize_events`:
/// `executions:read` on the run's project) and again at every keep-alive.
async fn events(req: &Request, env: &Env, path: &str) -> Result<Response> {
    ensure_schema(env).await?;
    let header = req.headers().get("last-event-id")?;
    let after = match runbook_api::sse::cursor(header.as_deref().map(Some)) {
        Ok(after) => after,
        Err(code) => return problem(400, code),
    };
    let host = WorkersHost::new(env, WorkersObjects::remote(env)?)?;
    let backends = CfBackends::new(host);
    // Parity (gate review): a bearer, or (with sign-in) the session cookie
    // the web UI streams with, decided as every other route decides it.
    let mut headers = Headers::new();
    for (name, value) in req.headers().entries() {
        headers.push(&name, value.as_bytes().to_vec());
    }
    let sessions = crate::auth::WorkersSessions::from_env(env, &origin(env));
    let presented = match runbook_api::session::credential(
        sessions.as_ref(),
        "GET",
        path,
        &headers,
        "req_events",
    )
    .await
    {
        Ok((Some(Credential::Bearer(secret)), _)) => Presented::Bearer(secret),
        Ok((Some(Credential::Session(_)), Some(cookie))) => Presented::Cookie(cookie),
        Ok(_) => Presented::Nothing,
        Err(response) => return to_response(response),
    };
    let authorize = {
        let backends = backends.clone();
        let path = path.to_owned();
        move |presented: Presented| {
            let backends = backends.clone();
            let sessions = sessions.clone();
            let path = path.clone();
            async move {
                // D32 (as native's `refresh_session`): a session stream asks
                // the auth Worker again (through its 30 s cache) and relinks
                // its user on every check; a signed-out session ends it.
                let credential = match presented {
                    Presented::Bearer(secret) => Some(Credential::Bearer(secret)),
                    Presented::Nothing => None,
                    Presented::Cookie(cookie) => {
                        use runbook_api::session::SessionResolver;
                        let sessions = sessions.ok_or((401, "auth.required"))?;
                        let mut session = match sessions.resolve(&cookie).await {
                            Ok(Some(session)) => session,
                            Ok(None) => return Err((401, "auth.required")),
                            Err(_) => return Err((503, "auth.unavailable")),
                        };
                        runbook_api::session::link(&backends, &mut session).await?;
                        Some(Credential::Session(session))
                    }
                };
                let service = runbook_api::executions::ExecutionService {
                    directory: backends.directory(),
                    credentials: backends.credentials(),
                    backend: backends.executions(),
                    origin: String::new(),
                };
                let request = runbook_api::Request {
                    method: "GET".into(),
                    path,
                    query: Default::default(),
                    body: serde_json::Value::Null,
                    credential,
                    idempotency_key: None,
                    request_id: "req_events".into(),
                    new_id: String::new(),
                    if_match: None,
                    client_ip: None,
                };
                service
                    .authorize_events(&request)
                    .await
                    .map_err(|e| (e.status, e.code))
            }
        }
    };
    let secret = presented;
    let (scope, execution_id) = match authorize(secret.clone()).await {
        Ok(found) => found,
        Err((status, code @ "auth.mfa_required")) => {
            return to_response(runbook_api::problem::Response::error(
                status,
                code,
                "req_events",
            ));
        }
        Err((status, code)) => return problem(status, code),
    };
    let stub = env
        .durable_object("EXECUTIONS")?
        .id_from_name(execution_id.as_str())?
        .get_stub()?;
    let mut init = worker::RequestInit::new();
    init.with_method(Method::Get);
    let mut upgrade = Request::new_with_init(
        &format!("https://do/{execution_id}/live?after={after}"),
        &init,
    )?;
    upgrade.headers_mut()?.set("upgrade", "websocket")?;
    let response = stub.fetch_with_request(upgrade).await?;
    if response.status_code() != 101 {
        return Ok(response);
    }
    let Some(socket) = response.websocket() else {
        return problem(503, "stream.unavailable");
    };
    let recheck: crate::live::Recheck = Box::new(move || {
        let check = authorize(secret.clone());
        let scope = scope.clone();
        Box::pin(async move { matches!(check.await, Ok((current, _)) if current == scope) })
    });
    let bridge = crate::live::Bridge::new(
        socket,
        // Native's fixed re-check and lifetime; test builds may shorten them.
        var_ms(env, "SSE_KEEPALIVE_MS", 30_000),
        var_ms(env, "SSE_LIFETIME_MS", 3_600_000),
        Some(recheck),
    )?;
    let mut response = Response::from_stream(bridge)?;
    for (name, value) in runbook_api::sse::HEADERS {
        response.headers_mut().set(name, value)?;
    }
    Ok(response)
}

/// What a stream request presented (re-checked on every keep-alive).
#[derive(Clone)]
enum Presented {
    Bearer(String),
    Cookie(String),
    Nothing,
}

/// A duration from a numeric var (milliseconds), or `default`.
/// Gate fix B1: a test seam, so always `default` in the deploy build.
fn var_ms(env: &Env, name: &str, default: u64) -> std::time::Duration {
    let ms = crate::seams::var(env, name)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default);
    std::time::Duration::from_millis(ms)
}

const BOOTSTRAP_SECRET_MIN: usize = 32;

/// The operator's first-run bootstrap: only with the `BOOTSTRAP_SECRET`
/// binding configured (absent otherwise: 404) and the same value in
/// `x-bootstrap-secret`, and only on an empty instance.
async fn bootstrap_route(mut req: Request, env: Env) -> Result<Response> {
    // Review 7: a secret shorter than 32 characters is no secret; the
    // route stays absent.
    let Some(expected) = env
        .secret("BOOTSTRAP_SECRET")
        .ok()
        .map(|s| s.to_string())
        .filter(|s| s.len() >= BOOTSTRAP_SECRET_MIN)
    else {
        return problem(404, "resource.not_found");
    };
    // Gate fix B4: the secret is compared first; only failures count, per
    // client address in the shared limiter, so guesses cannot lock the
    // operator out.
    let given = req.headers().get("x-bootstrap-secret")?.unwrap_or_default();
    // Compare digests so the comparison does not depend on where they differ.
    if secret_hash(&given).as_bytes() != secret_hash(&expected).as_bytes() {
        if let Err(retry) = secret_failed(&env, &req, "bootstrap").await {
            return limited(retry);
        }
        return problem(401, "auth.invalid_credential");
    }
    ensure_schema(&env).await?;
    // CF12b: read with the running cap every JSON route has.
    let crate::body::Capped::Body(body) = crate::body::read(&mut req, MAX_BODY_BYTES).await? else {
        return problem(413, "request.body_too_large");
    };
    let request: crate::bootstrap::BootstrapRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => return problem(422, "request.invalid"),
    };
    let host = WorkersHost::new(&env, WorkersObjects::remote(&env)?)?;
    match crate::bootstrap::run(&host, request).await {
        Ok(output) => Ok(Response::from_json(&output)?.with_status(201)),
        Err(code @ "bootstrap.already_initialized") => problem(409, code),
        Err(code) => problem(422, code),
    }
}

thread_local! {
    /// CF7: the auth Worker's sign-in methods, asked once per isolate.
    static AUTH_METHODS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// The sign-in methods for discovery: the auth Worker's `/internal/health`,
/// once per isolate (none without the binding; not cached while it fails).
async fn auth_methods(env: &Env) -> Vec<String> {
    if let Some(methods) = AUTH_METHODS.with(|m| m.borrow().clone()) {
        return methods;
    }
    let Some(sessions) = crate::auth::WorkersSessions::from_env(env, &origin(env)) else {
        return vec![];
    };
    let methods = sessions.methods().await;
    if !methods.is_empty() {
        AUTH_METHODS.with(|m| *m.borrow_mut() = Some(methods.clone()));
    }
    methods
}

/// D45 `POST /internal/can-register`, only for the auth Worker (its
/// `API` service binding presents the internal secret).
async fn can_register_route(mut req: Request, env: Env) -> Result<Response> {
    let origin = origin(&env);
    let Some(sessions) = crate::auth::WorkersSessions::from_env(&env, &origin) else {
        return problem(404, "route.not_found");
    };
    if !sessions.secret_matches(
        req.headers()
            .get(crate::auth::INTERNAL_SECRET_HEADER)?
            .as_deref(),
    ) {
        return problem(401, "auth.internal_unauthorized");
    }
    ensure_schema(&env).await?;
    let host = WorkersHost::new(&env, WorkersObjects::remote(&env)?)?;
    let request_id = nonce(&host)
        .map(|n| format!("req_{n}"))
        .unwrap_or_else(|_| "req_can_register".into());
    let path = req.url()?.path().to_owned();
    let incoming = read_incoming(&mut req, path, None).await?;
    let request = match to_request(&incoming, &request_id, String::new(), None) {
        Ok(request) => request,
        Err(response) => return to_response(response),
    };
    let router = Router::new(CfBackends::new(host), worker_router_config(&env));
    to_response(router.can_register(request).await)
}

/// API calls one `/mcp` message may make, and how long it may run (as the
/// native defaults, MCP1).
const MCP_CALLS_PER_MESSAGE: usize = 200;
const MCP_DEADLINE_MS: f64 = 30_000.0;

/// CF7: `/mcp` (MCP1) with the portable handler: the native prechecks, a
/// bearer only, and each tool call answered by the router here, which
/// reaches a run's ExecutionDO over the object protocol.
async fn mcp_route(mut req: Request, env: Env) -> Result<Response> {
    ensure_schema(&env).await?;
    let host = WorkersHost::new(&env, WorkersObjects::remote(&env)?)?;
    let Ok(tail) = nonce(&host) else {
        return problem(503, "server.entropy");
    };
    let id = format!("req_{tail}");
    let headers = crate::auth::runbook_headers(&req)?;
    let method = req.method().to_string();
    // CF12 review round 1: declared or chunked, never read past the cap.
    let crate::body::Capped::Body(body) = crate::body::read(&mut req, MAX_BODY_BYTES).await? else {
        return http_response(runbook_api::mcp::too_large(&id));
    };
    let router = Router::new(CfBackends::new(host), worker_router_config(&env));
    let deadline = js_sys::Date::now() + MCP_DEADLINE_MS;
    let expired = move || js_sys::Date::now() > deadline;
    // MCP2: OAuth access tokens are introspected by the auth Worker.
    let sessions = crate::auth::WorkersSessions::from_env(&env, &origin(&env));
    http_response(
        runbook_api::mcp::handle(
            &router,
            sessions.as_ref(),
            &method,
            &headers,
            &body,
            &id,
            MCP_CALLS_PER_MESSAGE,
            &expired,
        )
        .await,
    )
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    let app = origin(&env);
    let response = fetch_response(req, env, &app).await?;
    if !app.starts_with("https://") {
        return Ok(response);
    }
    let headers = worker::Headers::new();
    for (name, value) in response.headers().entries() {
        headers.append(&name, &value)?;
    }
    headers.set(
        "strict-transport-security",
        runbook_api::http::STRICT_TRANSPORT_SECURITY,
    )?;
    Ok(response.with_headers(headers))
}

async fn fetch_response(req: Request, env: Env, app: &str) -> Result<Response> {
    // Gate fix B1: the deploy build serves nothing while a test seam is set.
    if let Some(name) = crate::seams::set_in(&env) {
        worker::console_error!("config.test_seam_set: {name}");
        return to_response(ApiResponse::error(
            503,
            "config.test_seam_set",
            "urn:request:config",
        ));
    }
    // Gate round 2: an unbound rate limiter is a configuration error, not a
    // reason to serve without limits (a failed call still fails open).
    if env.durable_object("LIMITS").is_err() {
        worker::console_error!("config.limits_unbound");
        return to_response(ApiResponse::error(
            503,
            "config.limits_unbound",
            "urn:request:config",
        ));
    }
    // UI-1 review: a theme colour that is not `#rrggbb` is logged and the
    // whole override ignored (the default theme is served; see
    // `worker_router_config`). Natively it refuses startup, but a Worker
    // has no startup: refusing here would turn a cosmetic typo into every
    // request failing. Low contrast is only logged. Once per isolate.
    if !THEME_CHECKED.with(|c| c.replace(true)) {
        match theme(&env) {
            Err(error) => {
                worker::console_error!("{error}; the theme override is ignored");
            }
            Ok(_) => {
                for warning in runbook_api::discovery::theme_warnings(|name| var(&env, name)) {
                    worker::console_log!("{warning}");
                }
            }
        }
    }
    // CF7 (D50): the user-content origin serves only `/u/*`.
    if usercontent::addressed(&env, app, &req) {
        return usercontent::serve(req, env, app.to_owned()).await;
    }
    let url = req.url()?;
    let path = url.path().to_owned();
    let query = url.query().map(str::to_owned);
    match (req.method(), path.as_str()) {
        (Method::Get, "/health") => Response::ok("ok"),
        (Method::Post, "/internal/bootstrap") => bootstrap_route(req, env).await,
        // CF7: the auth Worker behind `AUTH` (D43); without it, no sign-in.
        // MCP2: it also answers the OAuth discovery documents; without it
        // those are the API's unknown routes, as natively.
        (_, p)
            if p == "/api/auth"
                || p.starts_with("/api/auth/")
                || runbook_api::oauth::proxied(p) =>
        {
            match crate::auth::WorkersSessions::from_env(&env, &origin(&env)) {
                Some(sessions) => sessions.proxy(req).await,
                None if p.starts_with("/.well-known/") => api(req, env, path, query).await,
                None => problem(503, "auth.not_configured"),
            }
        }
        (Method::Post, runbook_api::session::CAN_REGISTER | runbook_api::mfa::INTERNAL_POLICY) => {
            can_register_route(req, env).await
        }
        (_, runbook_api::mcp::PATH) => mcp_route(req, env).await,
        // CF8: the operator's project import and export (absent unless
        // RUNBOOK_OPERATOR_SECRET is bound).
        (_, operator::IMPORT | operator::EXPORT) => operator::serve(req, env).await,
        (Method::Get, p)
            if runbook_api::directory::match_path("/v1/executions/{exe}/events", p).is_some() =>
        {
            events(&req, &env, p).await
        }
        // CF7 (B2): the web UI, when the Worker has its static assets.
        (ref method, p) if web::serves(method, p) => match env.assets("ASSETS") {
            Ok(assets) => web::serve(assets, req).await,
            Err(_) => api(req, env, path, query).await,
        },
        // Gate fix B2: native's per-IP limit on new device codes (AUTH3).
        (Method::Post, runbook_api::device::CODE) => {
            let key = format!("code:{}", client_ip(&req));
            match crate::limits::RateLimits::hit(
                &DoLimits::new(&env),
                &key,
                3_600_000,
                runbook_api::device::CODES_PER_HOUR,
                js_sys::Date::now() as i64,
            )
            .await
            {
                Ok(()) => api(req, env, path, query).await,
                Err(retry) => limited(retry),
            }
        }
        _ => api(req, env, path, query).await,
    }
}

/// The client's rate-limit key from the address Cloudflare reports: an
/// IPv6 client by its /64 (gate round 2), or `unknown`.
pub(crate) fn client_ip(req: &Request) -> String {
    req.headers()
        .get("cf-connecting-ip")
        .ok()
        .flatten()
        .map_or_else(|| "unknown".into(), |ip| crate::limits::client_key(&ip))
}

/// AUTH3's 429, as natively: `limit.rate` with `retry_after` seconds.
pub(crate) fn limited(retry: u64) -> Result<Response> {
    let mut response = ApiResponse::error(429, "limit.rate", "urn:request:limit");
    response.body["retry_after"] = json!(retry);
    let mut response = to_response(response)?;
    response
        .headers_mut()
        .set("retry-after", &retry.to_string())?;
    Ok(response)
}

/// Gate fix B4: a wrong operator or bootstrap secret counts against the
/// client's address (a few a minute); a right one never does, so guessing
/// cannot lock the operator out. `Err` is the 429 once over.
pub(crate) async fn secret_failed(
    env: &Env,
    req: &Request,
    route: &str,
) -> std::result::Result<(), u64> {
    crate::limits::RateLimits::hit(
        &DoLimits::new(env),
        &format!("{route}-failure:{}", client_ip(req)),
        60_000,
        5,
        js_sys::Date::now() as i64,
    )
    .await
}

// ---------------------------------------------------------------------------
// The ExecutionDO.

/// One request at a time per object, held across the router's awaits
/// (CF2-5): a waiter queue, since a DO is single-threaded.
#[derive(Default)]
struct Gate {
    busy: Cell<bool>,
    waiters: RefCell<VecDeque<Waker>>,
}
struct GateGuard<'a>(&'a Gate);
impl Gate {
    async fn lock(&self) -> GateGuard<'_> {
        std::future::poll_fn(|cx| {
            if self.busy.get() {
                self.waiters.borrow_mut().push_back(cx.waker().clone());
                Poll::Pending
            } else {
                self.busy.set(true);
                Poll::Ready(())
            }
        })
        .await;
        GateGuard(self)
    }
}
impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        self.0.busy.set(false);
        if let Some(waker) = self.0.waiters.borrow_mut().pop_front() {
            waker.wake();
        }
    }
}

/// Fault names the spike tests inject, accepted only under the test var.
#[cfg(feature = "test-seams")]
fn parse_fault(text: &str) -> Option<Fault> {
    Some(match text {
        "after_events" => Fault::After(WriteStage::Events),
        "sql_constraint" => Fault::SqlConstraint,
        "after_alarm" => Fault::AfterAlarm,
        "panic" => Fault::Panic,
        _ => return None,
    })
}
fn code(error: &StoreError) -> String {
    SpikeError::Store(error.clone()).code()
}
fn store_problem(error: &StoreError) -> Result<Response> {
    match error {
        StoreError::NotFound => problem(404, "execution.not_found"),
        StoreError::InvalidBatch(code) => problem(409, code),
        StoreError::Backend(_) => problem(500, &code(error)),
    }
}

#[durable_object]
pub struct ExecutionDO {
    state: State,
    env: Env,
    object: Rc<DoObject<WorkersSql>>,
    gate: Rc<Gate>,
}

impl ExecutionDO {
    /// Test seam (local harness only, CF8): hold every run's background
    /// work (the settle after a request or an alarm, and publishing after an
    /// object-protocol write), so an imported project can be exported again
    /// before its runs resume (outbox delivery, due timers).
    fn work_held(&self) -> bool {
        crate::seams::on(&self.env, "RUNBOOK_TEST_HOLD_WORK")
    }
    fn test_hooks(&self) -> bool {
        crate::seams::on(&self.env, "CF1_FAULT_INJECTION")
    }
    fn backends(&self, id: &ExecutionId) -> Result<CfBackends<WorkersHost>> {
        Ok(CfBackends::new(WorkersHost::new(
            &self.env,
            WorkersObjects::own(&self.env, self.object.clone(), id)?,
        )?))
    }
    /// The tenant of this object's run, from its own head (review fix 4:
    /// no D1 read). `None` for an object that holds no run, or another one.
    fn scope(&self, id: &ExecutionId) -> Option<TenantScope> {
        match self.object.stored_owner() {
            Ok(Some((stored, scope))) if &stored == id => Some(scope),
            _ => None,
        }
    }
    /// Send the notification stream to the queue (CF4).
    async fn publish(&self, store: &DoStore<WorkersSql>) {
        if let Ok(queue) = self.env.queue(crate::notify::BINDING) {
            let _ = crate::delivery::publish(store, &crate::notify::WorkersQueue(queue)).await;
        }
    }
    /// After a request or an alarm: settle (which tells promoted waiters'
    /// objects, CF5), live frames and notifications. `Err` when the settle
    /// did not run, so an alarm retries (review fix 4).
    async fn after(&self, id: &ExecutionId) -> std::result::Result<(), String> {
        if self.work_held() {
            return Ok(());
        }
        let Some(scope) = self.scope(id) else {
            return Ok(());
        };
        let store = self.object.store(id, scope.clone()).map_err(|e| code(&e))?;
        let settled = match self.backends(id) {
            Ok(backends) => settle(&backends, id, &scope).await.map_err(|e| code(&e)),
            Err(error) => Err(format!("binding: {error}")),
        };
        // Waiters this settle promoted were told through the object
        // protocol (CF5); the minute cron retries what failed.
        crate::live::fan_out(&self.state, &store).await;
        self.publish(&store).await;
        settled.map(|_| ())
    }

    async fn api(
        &self,
        mut req: Request,
        id: &ExecutionId,
        path: String,
        query: Option<String>,
    ) -> Result<Response> {
        let _turn = self.gate.lock().await;
        ensure_schema(&self.env).await?;
        let new_id = req.headers().get(NEW_ID)?.unwrap_or_default();
        let request_id = req
            .headers()
            .get(REQUEST_ID)?
            .unwrap_or_else(|| "req_object".into());
        let incoming = read_incoming(&mut req, path, query).await?;
        let credential = match credential(&self.env, &incoming, &request_id).await {
            Ok(credential) => credential,
            Err(response) => return to_response(response),
        };
        let response = match to_request(&incoming, &request_id, new_id, credential) {
            Ok(request) => {
                let router = Router::new(self.backends(id)?, worker_router_config(&self.env));
                router.handle(request).await
            }
            Err(response) => response,
        };
        // A failed settle leaves its work to the alarm (the drain deferred
        // its stream, which arms it).
        let _ = self.after(id).await;
        // Marks an answer an object produced, so tests can tell it from
        // the api Worker refusing an ID before any object (§18). The
        // Worker strips `x-runbook-*` from what clients send, not from this.
        let mut response = to_response(response)?;
        response.headers_mut().set(OBJECT_HEADER, "1")?;
        Ok(response)
    }
}

// ---- CF1/CF2 spike hooks: the test build only (gate fix B1) ----
#[cfg(feature = "test-seams")]
impl ExecutionDO {
    async fn start(&self, store: &DoStore<WorkersSql>, fault: Option<Fault>) -> Result<Response> {
        if let Some(fault) = fault {
            store.inject(fault);
        }
        match spike::start_with(store, store.name(), store.now_ms()).await {
            Ok(CommitResult::Committed(sequence)) => {
                crate::live::fan_out(&self.state, store).await;
                self.publish(store).await;
                Response::from_json(&json!({ "committed": sequence }))
            }
            Ok(CommitResult::Conflict) => problem(409, "store.conflict"),
            Err(error) => problem(500, &error.code()),
        }
    }
    async fn dump(&self, store: &DoStore<WorkersSql>) -> Result<Response> {
        use runbook_core::ExecutionStore;
        let mut counts = serde_json::Map::new();
        for (table, n) in store
            .table_counts()
            .map_err(|e| worker::Error::from(code(&e)))?
        {
            counts.insert(table.into(), n.into());
        }
        let alarm = self.state.storage().get_alarm().await?;
        let run = match store.current_execution() {
            Ok(Some(id)) => match store.load(&id).await {
                Ok(run) => Some(run),
                Err(error) => return problem(500, &code(&error)),
            },
            Ok(None) => None,
            Err(error) => return problem(500, &code(&error)),
        };
        #[derive(Deserialize, Serialize)]
        struct AlarmLog {
            fired_at_ms: i64,
            outcome: String,
        }
        let alarms: Vec<AlarmLog> = self
            .state
            .storage()
            .sql()
            .exec(
                "SELECT fired_at_ms, outcome FROM alarm_log ORDER BY id",
                None,
            )?
            .to_array()?;
        let pending = match &run {
            Some(_) => json!({
                "derived": store.pending_derived(None, 100)
                    .map_err(|e| worker::Error::from(code(&e)))?
                    .iter().map(|(sequence, _)| *sequence).collect::<Vec<_>>(),
                "notification": store.pending_notifications(None, 100)
                    .map_err(|e| worker::Error::from(code(&e)))?
                    .iter().map(|intent| intent.sequence).collect::<Vec<_>>(),
            }),
            None => json!(null),
        };
        Response::from_json(&json!({
            "counts": counts,
            "alarm_at_ms": alarm,
            "next_alarm_at_ms": next_alarm_at(store).map_err(|e| worker::Error::from(code(&e)))?,
            "alarm_log": alarms,
            "pending": pending,
            "run": run.map(|run| json!({
                "revision": run.revision,
                "status": run.projection.status,
                "events": run.events.iter().map(spike::event_type).collect::<Vec<_>>(),
                "steps": run.projection.steps.iter()
                    .map(|s| json!({ "path": s.path, "status": s.status }))
                    .collect::<Vec<_>>(),
                "timers": run.timers.values()
                    .map(|t| json!({ "key": t.key, "due_at_ms": t.due_at_ms }))
                    .collect::<Vec<_>>(),
            })),
        }))
    }
    fn outbox(
        &self,
        store: &DoStore<WorkersSql>,
        stream: OutboxStream,
        ack: Option<(u64, Option<String>)>,
    ) -> Result<Response> {
        let outcome = match (stream, ack) {
            (OutboxStream::Derived, Some((through, _))) => store
                .ack_derived(through)
                .map(|acked| json!({ "acked": acked })),
            (OutboxStream::Notification, Some((sequence, Some(event_id)))) => store
                .ack_notifications(&NotificationCursor { sequence, event_id })
                .map(|acked| json!({ "acked": acked })),
            (OutboxStream::Notification, Some((_, None))) => {
                return problem(400, "outbox.invalid_cursor");
            }
            (OutboxStream::Derived, None) => store
                .pending_derived(None, 100)
                .map(|rows| json!({ "pending": rows.iter().map(|(n, _)| *n).collect::<Vec<_>>() })),
            (OutboxStream::Notification, None) => {
                store.pending_notifications(None, 100).map(|rows| {
                    json!({ "pending": rows.iter()
                    .map(|i| json!({ "sequence": i.sequence, "event_id": i.event_id }))
                    .collect::<Vec<_>>() })
                })
            }
        };
        match outcome {
            Ok(body) => Response::from_json(&body),
            Err(error) => store_problem(&error),
        }
    }
}

/// LANG9b: the minute cron's schedule pass. The due ticks come from D1;
/// each starts in a new run's own object, as an API start does.
async fn schedule_pass(env: &Env) -> serde_json::Value {
    let host = match WorkersObjects::remote(env).and_then(|objects| WorkersHost::new(env, objects))
    {
        Ok(host) => host,
        Err(error) => return json!({ "error": format!("binding: {error}") }),
    };
    let backends = CfBackends::new(host.clone());
    let mut pass = runbook_api::triggers::SchedulePass::default();
    let due = match runbook_api::triggers::due_schedules(&backends, host.now_ms(), &mut pass).await
    {
        Ok(due) => due,
        Err(error) => return json!({ "error": code(&error) }),
    };
    let origin = origin(env);
    for start in due {
        let Some(exe) = nonce(&host)
            .ok()
            .and_then(|id| format!("exe_{id}").parse::<ExecutionId>().ok())
        else {
            pass.failed
                .push((start.runbook_id.to_string(), "server.entropy"));
            continue;
        };
        let body = runbook_api::triggers::ScheduledStart {
            start: start.clone(),
            origin: origin.clone(),
        };
        let sent = async {
            let stub = env
                .durable_object("EXECUTIONS")?
                .id_from_name(exe.as_str())?
                .get_stub()?;
            let mut init = worker::RequestInit::new();
            init.with_method(Method::Post);
            init.with_body(Some(serde_json::to_string(&body)?.into()));
            let request = Request::new_with_init(&format!("https://do/{exe}/schedule"), &init)?;
            stub.fetch_with_request(request).await
        }
        .await;
        match sent {
            Ok(response) if response.status_code() < 300 => pass.started += 1,
            Ok(response) => pass.failed.push((
                start.runbook_id.to_string(),
                if response.status_code() == 409 {
                    "start.conflict"
                } else {
                    "trigger.start_failed"
                },
            )),
            Err(_) => pass
                .failed
                .push((start.runbook_id.to_string(), "trigger.unreachable")),
        }
    }
    for (runbook, failure) in &pass.failed {
        worker::console_error!("triggers.schedule: {runbook}: {failure}");
    }
    json!({ "scheduled": pass.scheduled, "started": pass.started, "failed": pass.failed.len() })
}

/// How long telling another object about its admission may take (it may
/// be waiting on this object's gate, or be unreachable).
const NOTICE_TIMEOUT_MS: u64 = 5_000;

/// Tell a promoted waiter's object it is admitted (D24): its HTTP status,
/// or `None` on a timeout or transport failure.
async fn notify_admitted(
    env: &Env,
    notice: &runbook_store_d1::admission::Promotion,
) -> Option<u16> {
    let stub = env
        .durable_object("EXECUTIONS")
        .ok()?
        .id_from_name(notice.execution_id.as_str())
        .ok()?
        .get_stub()
        .ok()?;
    let mut init = worker::RequestInit::new();
    init.with_method(Method::Post);
    let request = Request::new_with_init(
        &format!(
            "https://do/{}/admit?at={}",
            notice.execution_id, notice.admitted_at_ms
        ),
        &init,
    )
    .ok()?;
    match futures_util::future::select(
        Box::pin(stub.fetch_with_request(request)),
        Box::pin(worker::Delay::from(std::time::Duration::from_millis(
            NOTICE_TIMEOUT_MS,
        ))),
    )
    .await
    {
        futures_util::future::Either::Left((Ok(response), _)) => Some(response.status_code()),
        _ => None,
    }
}

/// Deliver one notice (review fix 6). The object deletes the row once it
/// applied it (200). A terminal refusal also ends it here: 404 (the object
/// holds no run, or another) or 409 (the run already started, or refuses
/// the time). Anything else leaves the row for the next pass.
async fn deliver_notice(
    env: &Env,
    db: &WorkersD1,
    notice: &runbook_store_d1::admission::Promotion,
) -> bool {
    match notify_admitted(env, notice).await {
        Some(200) => true,
        // Gate fix D1: an empty object may still be initialized by its
        // half-committed start's retry; the notice stays until the run
        // has no owner row or is tombstoned.
        Some(404)
            if !runbook_store_d1::admission::notice_orphaned(db, &notice.execution_id)
                .await
                .unwrap_or(false) =>
        {
            false
        }
        Some(404 | 409) => {
            let _ = runbook_store_d1::admission::ack_notice(db, notice).await;
            true
        }
        _ => false,
    }
}

impl DurableObject for ExecutionDO {
    fn new(state: State, env: Env) -> Self {
        let object = Rc::new(DoObject::new(WorkersSql::new(state.storage())));
        Self {
            state,
            env,
            object,
            gate: Rc::default(),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let url = req.url()?;
        let segments: Vec<String> = url
            .path_segments()
            .map(|s| s.map(str::to_owned).collect())
            .unwrap_or_default();
        let query_of = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        let Some((id, rest)) = segments.split_first() else {
            return problem(404, "route.not_found");
        };
        let Ok(id) = id.parse::<ExecutionId>() else {
            return problem(400, "execution.invalid_id");
        };
        let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
        match (req.method(), rest.as_slice()) {
            (_, ["api", ..]) => {
                let path = format!("/{}", rest[1..].join("/"));
                let query = url.query().map(str::to_owned);
                self.api(req, &id, path, query).await
            }
            // LANG9b: the minute cron starts a due schedule tick as this
            // object's run. Objects are reachable only from this Worker.
            (Method::Post, ["schedule"]) => {
                let mut req = req;
                let _turn = self.gate.lock().await;
                ensure_schema(&self.env).await?;
                let Ok(body) = req.json::<runbook_api::triggers::ScheduledStart>().await else {
                    return problem(400, "request.invalid");
                };
                let backends = self.backends(&id)?;
                let vars = match runbook_api::triggers::project_vars(&backends, &body.start).await {
                    Ok(vars) => vars,
                    Err(_) => return problem(503, "storage.unavailable"),
                };
                let invocation = body.start.invocation(id.clone(), &body.origin, vars);
                let started = runbook_api::executions::ExecutionBackend::start(
                    &backends.executions(),
                    &body.start.scope(),
                    invocation,
                )
                .await;
                let _ = self.after(&id).await;
                match started {
                    Ok(_) => Response::from_json(&json!({ "started": true })),
                    Err(error) => problem(error.status, error.code),
                }
            }
            // D58: a live subscriber (the api Worker's SSE bridge), for the
            // run's verified tenant.
            (Method::Get, ["live"]) => {
                let after = query_of("after")
                    .and_then(|a| a.parse::<u64>().ok())
                    .unwrap_or(0);
                let Some(scope) = self.scope(&id) else {
                    return problem(404, "resource.not_found");
                };
                let store = match self.object.store(&id, scope) {
                    Ok(store) => store,
                    Err(error) => return store_problem(&error),
                };
                match crate::live::subscribe(&self.state, &store, after).await {
                    Ok(client) => Response::from_websocket(client),
                    Err((status, code)) => problem(status, code),
                }
            }
            // D24: a waiter promoted in D1 (from another object or the cron).
            // An object with no run (404) or a run that refuses the time
            // (409) is a terminal answer: the caller drops the notice.
            (Method::Post, ["admit"]) => {
                let Some(at) = query_of("at").and_then(|a| a.parse::<i64>().ok()) else {
                    return problem(400, "admission.invalid_time");
                };
                let _turn = self.gate.lock().await;
                let Some(scope) = self.scope(&id) else {
                    return problem(404, "resource.not_found");
                };
                let store = match self.object.store(&id, scope.clone()) {
                    Ok(store) => store,
                    Err(error) => return store_problem(&error),
                };
                if let Err(error) = store.admit(&id, at) {
                    return store_problem(&error);
                }
                let db = env_shards(&self.env)?.for_org(&scope.org_id);
                let _ = runbook_store_d1::admission::ack_notice(
                    &db,
                    &runbook_store_d1::admission::Promotion {
                        execution_id: id.clone(),
                        admitted_at_ms: at,
                    },
                )
                .await;
                let _ = self.after(&id).await;
                Response::ok("admitted")
            }
            // CF5: the object protocol. Another host (another run's object,
            // the api Worker's cron) operates on this run's store for the
            // tenant its verified credential named; the store refuses any
            // other tenant. After a write, new frames go to live subscribers
            // and notifications to the queue; the alarm settles the rest, so
            // a call never waits on another object's gate.
            (Method::Post, ["op"]) => {
                let mut req = req;
                ensure_schema(&self.env).await?;
                // `serde_json` on the text, never `Request::json` (JS
                // `JSON.parse`, then `serde_wasm_bindgen`), which loses
                // `serde_json`'s arbitrary-precision numbers in run data.
                let request: crate::protocol::Request =
                    match serde_json::from_str(&req.text().await?) {
                        Ok(request) => request,
                        Err(_) => return problem(400, "object.invalid_request"),
                    };
                if request.execution_id != id {
                    return problem(400, "object.invalid_request");
                }
                let writes = crate::protocol::writes(&request.op);
                let reply = match self.object.store(&id, request.scope) {
                    Ok(store) => {
                        let reply = crate::protocol::serve(&store, request.op, false).await;
                        if writes && reply.is_ok() && !self.work_held() {
                            crate::live::fan_out(&self.state, &store).await;
                            self.publish(&store).await;
                        }
                        reply
                    }
                    Err(StoreError::Backend("do.object_mismatch")) => Err(StoreError::NotFound),
                    Err(error) => Err(error),
                };
                Response::from_json(&crate::protocol::into_wire(reply))
            }
            // Review fix 2: the minute Cron asks about a start that never
            // reached D1. An initialized run is drained; an empty object's
            // reservation is abandoned in D1.
            (Method::Post, ["reconcile"]) => {
                let _turn = self.gate.lock().await;
                ensure_schema(&self.env).await?;
                let scope = match self.scope(&id) {
                    Some(scope) => scope,
                    None => {
                        // The cron names the run's organization (its shard);
                        // without it, each shard is asked (CF11).
                        let shards = env_shards(&self.env)?;
                        let dbs = match query_of("org").and_then(|o| o.parse().ok()) {
                            Some(org) => vec![shards.for_org(&org)],
                            None => shards.data_shards().into_iter().map(|(_, db)| db).collect(),
                        };
                        let mut found = None;
                        for db in dbs {
                            match runbook_store_d1::catalog::execution_owner(&db, &id).await {
                                Ok(Some(scope)) => {
                                    found = Some(scope);
                                    break;
                                }
                                Ok(None) => {}
                                Err(error) => return store_problem(&error),
                            }
                        }
                        match found {
                            Some(scope) => scope,
                            None => return problem(404, "resource.not_found"),
                        }
                    }
                };
                let store = self.backends(&id)?.execution(scope);
                match crate::reconcile::one(&store, &id).await {
                    Ok(outcome) => {
                        Response::from_json(&json!({ "outcome": format!("{outcome:?}") }))
                    }
                    Err(error) => store_problem(&error),
                }
            }
            // CF1/CF2 spike hooks: local test var only, test build only.
            #[cfg(feature = "test-seams")]
            (method, hook) if self.test_hooks() => {
                let store = match self.object.store(&id, spike::scope()) {
                    Ok(store) => store,
                    Err(error) => return store_problem(&error),
                };
                match (method, hook) {
                    (Method::Post, ["start"]) => {
                        let fault = match query_of("fault") {
                            None => None,
                            Some(name) => match parse_fault(&name) {
                                Some(fault) => Some(fault),
                                None => return problem(400, "cf1.unknown_fault"),
                            },
                        };
                        self.start(&store, fault).await
                    }
                    (Method::Get, ["state"]) => self.dump(&store).await,
                    (method, ["outbox", stream, tail @ ..]) => {
                        let Some(stream) = OutboxStream::parse(stream) else {
                            return problem(404, "route.not_found");
                        };
                        match (method, tail) {
                            (Method::Get, []) => self.outbox(&store, stream, None),
                            (Method::Post, ["ack"]) => match query_of("through")
                                .and_then(|t| t.parse::<u64>().ok())
                            {
                                Some(through) => {
                                    self.outbox(&store, stream, Some((through, query_of("event"))))
                                }
                                None => problem(400, "outbox.invalid_cursor"),
                            },
                            _ => problem(404, "route.not_found"),
                        }
                    }
                    _ => problem(404, "route.not_found"),
                }
            }
            _ => problem(404, "route.not_found"),
        }
    }

    async fn websocket_message(
        &self,
        _ws: worker::WebSocket,
        _message: worker::WebSocketIncomingMessage,
    ) -> Result<()> {
        Ok(())
    }
    async fn websocket_close(
        &self,
        ws: worker::WebSocket,
        code: usize,
        reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        let code = u16::try_from(code)
            .ok()
            .filter(|c| *c >= 1000)
            .unwrap_or(1000);
        let _ = ws.close(Some(code), Some(reason));
        Ok(())
    }
    async fn websocket_error(&self, _ws: worker::WebSocket, _error: worker::Error) -> Result<()> {
        Ok(())
    }

    /// Settle the run (core's drain for due work, then derived rows into
    /// D1), fan out and publish, for the tenant the object's own head names
    /// (review fix 4: no D1 read decides it). A settle that did not run is
    /// an `Err`, logged, so workerd retries the alarm with backoff. CF1's
    /// spike handler runs only under the local test var, for a spike run.
    async fn alarm(&self) -> Result<Response> {
        let outcome = match self.object.stored_owner() {
            Ok(None) => Ok("no_run".to_owned()),
            #[cfg(feature = "test-seams")]
            Ok(Some((id, scope))) if self.test_hooks() && scope == spike::scope() => {
                match self.object.store(&id, scope) {
                    Ok(store) => {
                        self.publish(&store).await;
                        let outcome = spike::run_alarm(&store).await;
                        crate::live::fan_out(&self.state, &store).await;
                        self.publish(&store).await;
                        outcome
                    }
                    Err(error) => Err(code(&error)),
                }
            }
            Ok(Some((id, scope))) => {
                let _turn = self.gate.lock().await;
                // Gate round 2: a held run wakes only to expire its hold.
                match self.object.store(&id, scope.clone()).and_then(|s| s.held()) {
                    Ok(true) => match self.backends(&id) {
                        Ok(backends) => {
                            crate::portable::expire_hold(&backends.execution(scope), &id)
                                .await
                                .map(|()| "hold expired".to_owned())
                                .map_err(|e| code(&e))
                        }
                        Err(_) => Err("object.unconfigured".to_owned()),
                    },
                    _ => self.after(&id).await.map(|()| "settled".to_owned()),
                }
            }
            Err(error) => Err(code(&error)),
        };
        let at_ms = WorkersSql::new(self.state.storage()).now_ms();
        let logged = match &outcome {
            Ok(text) => text.clone(),
            Err(code) => format!("retry:{code}"),
        };
        self.state.storage().sql().exec(
            "INSERT INTO alarm_log (fired_at_ms, outcome) VALUES (?, ?)",
            vec![SqlStorageValue::from(at_ms), logged.into()],
        )?;
        match outcome {
            Ok(_) => Response::ok("alarm"),
            Err(code) => Err(worker::Error::from(code)),
        }
    }
}

// ---------------------------------------------------------------------------
// CF6: the OrgDO, one per organization: its execution quota counters.

#[durable_object]
pub struct OrgDO {
    state: State,
}

impl DurableObject for OrgDO {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }
    /// `POST /quota`: one quota operation, only from Worker code (the api
    /// Worker never forwards a client here). Each is one transaction in a
    /// single-threaded object, so concurrent starts cannot overshoot.
    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        if req.method() != Method::Post || url.path() != "/quota" {
            return problem(404, "route.not_found");
        }
        let request: crate::quota::QuotaRequest = match serde_json::from_str(&req.text().await?) {
            Ok(request) => request,
            Err(_) => return problem(400, "quota.invalid_request"),
        };
        let reply = runbook_store_do::org::OrgStore::open(WorkersSql::new(self.state.storage()))
            .and_then(|store| crate::quota::serve_quota(&store, request));
        Response::from_json(&crate::protocol::into_wire(reply))
    }
}

/// CF11: one webhook subscription's delivery log (deliveries, attempts,
/// retry and backoff state), moved out of D1. `POST /op` serves the webhook
/// protocol (only Worker code reaches it); after each request and alarm the
/// object arms its alarm for its next due delivery, and the alarm attempts
/// its own due deliveries, as the native drain's redelivery does.
/// Gate fix B2: the AUTH3 rate-limit windows, one LimitsDO per key, so
/// every isolate counts the same hits. A limiter that cannot be reached
/// lets the request through (it never locks people out) and says so.
#[derive(Clone)]
pub struct DoLimits(Option<worker::ObjectNamespace>);
impl DoLimits {
    pub(crate) fn new(env: &Env) -> Self {
        Self(env.durable_object("LIMITS").ok())
    }
    async fn ask(&self, key: &str, window_ms: u64, limit: u32, now_ms: i64) -> Result<Option<u64>> {
        let Some(namespace) = &self.0 else {
            return Err(worker::Error::from("limits.unbound"));
        };
        let stub = namespace.id_from_name(key)?.get_stub()?;
        let mut init = worker::RequestInit::new();
        init.with_method(Method::Post).with_body(Some(
            json!({ "window_ms": window_ms, "limit": limit, "now_ms": now_ms })
                .to_string()
                .into(),
        ));
        let mut response = stub
            .fetch_with_request(Request::new_with_init("https://limits/hit", &init)?)
            .await?;
        let body: serde_json::Value = serde_json::from_str(&response.text().await?)
            .map_err(|_| worker::Error::from("limits.decode"))?;
        Ok(body.get("retry_after").and_then(serde_json::Value::as_u64))
    }
}
impl crate::limits::RateLimits for DoLimits {
    async fn hit(
        &self,
        key: &str,
        window_ms: u64,
        limit: u32,
        now_ms: i64,
    ) -> std::result::Result<(), u64> {
        match self.ask(key, window_ms, limit, now_ms).await {
            Ok(Some(retry)) => Err(retry),
            Ok(None) => Ok(()),
            Err(error) => {
                worker::console_error!("limits.unavailable: {error}");
                Ok(())
            }
        }
    }
}

/// One rate-limit key's window (gate fix B2). Its storage holds the hits;
/// an alarm one window after the last hit forgets them.
#[durable_object]
pub struct LimitsDO {
    state: State,
}
impl DurableObject for LimitsDO {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }
    async fn fetch(&self, mut req: Request) -> Result<Response> {
        #[derive(serde::Deserialize)]
        struct Hit {
            window_ms: u64,
            limit: u32,
            now_ms: i64,
        }
        let Ok(hit) = serde_json::from_str::<Hit>(&req.text().await?) else {
            return problem(400, "limits.invalid_request");
        };
        let storage = self.state.storage();
        let mut hits: Vec<i64> = storage.get("hits").await.ok().flatten().unwrap_or_default();
        let outcome = crate::limits::slide(&mut hits, hit.window_ms, hit.limit, hit.now_ms);
        storage.put("hits", &hits).await?;
        let window = i64::try_from(hit.window_ms).unwrap_or(i64::MAX);
        storage
            .set_alarm(js_sys::Date::now() as i64 + window.min(86_400_000))
            .await?;
        match outcome {
            Ok(()) => Response::from_json(&json!({ "ok": true })),
            Err(retry) => Response::from_json(&json!({ "retry_after": retry })),
        }
    }
    async fn alarm(&self) -> Result<Response> {
        self.state.storage().delete_all().await?;
        Response::ok("forgotten")
    }
}

#[durable_object]
pub struct WebhookDO {
    state: State,
    env: Env,
}

impl WebhookDO {
    fn sql(&self) -> WorkersSql {
        WorkersSql::new(self.state.storage())
    }
    /// The alarm at the log's next due delivery, or none: never sooner
    /// than a second from now, and backed off after failed passes (gate
    /// fix D3, `webhook::alarm_at`).
    fn rearm(&self, result: runbook_store_do::webhook::AlarmResult) {
        let sql = self.sql();
        let due = runbook_store_do::webhook::stored_owner(&sql)
            .ok()
            .flatten()
            .and_then(|(scope, id)| {
                runbook_store_do::webhook::WebhookLog::open(sql.clone(), scope, id).ok()
            })
            .and_then(|log| log.next_due().ok())
            .flatten();
        let next = runbook_store_do::webhook::alarm_at(&sql, due, sql.now_ms(), result)
            .unwrap_or(due.map(|d| d.max(sql.now_ms() + 60_000)));
        let _ = sql.set_alarm(next);
    }
}

impl DurableObject for WebhookDO {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }
    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        if req.method() != Method::Post || url.path() != "/op" {
            return problem(404, "route.not_found");
        }
        let request: crate::webhooks::WebhookRequest =
            match serde_json::from_str(&req.text().await?) {
                Ok(request) => request,
                Err(_) => return problem(400, "webhooks.invalid_request"),
            };
        let reply = crate::webhooks::serve_webhook(self.sql(), request);
        self.rearm(runbook_store_do::webhook::AlarmResult::Rearm);
        Response::from_json(&crate::webhooks::encode_webhook_reply(&reply))
    }
    async fn alarm(&self) -> Result<Response> {
        let sql = self.sql();
        let owner = runbook_store_do::webhook::stored_owner(&sql).ok().flatten();
        let outcome = match (owner, env_shards(&self.env), DoTransport::new(&self.env)) {
            (Some((scope, id)), Ok(shards), Ok(transport)) => {
                let objects = crate::webhooks::LocalFirst {
                    subscription: id.clone(),
                    sql: sql.clone(),
                    rest: crate::webhooks::ViaTransport(transport),
                };
                let webhooks =
                    crate::webhooks::CfWebhooks::sharded(shards, objects).only(id.clone());
                let fetch = crate::notify::FetchTransport {
                    policy: crate::notify::policy(&self.env),
                };
                let sealer = crate::notify::sealer(&self.env);
                crate::webhooks::alarm(&webhooks, &fetch, sealer.as_ref(), &scope, &id).await
            }
            (None, ..) => Ok("empty".to_owned()),
            _ => Err("webhooks.unconfigured".to_owned()),
        };
        if let Err(code) = &outcome {
            worker::console_error!("webhook alarm failed: {code}");
        }
        self.rearm(if outcome.is_ok() {
            runbook_store_do::webhook::AlarmResult::Succeeded
        } else {
            runbook_store_do::webhook::AlarmResult::Failed
        });
        Response::ok(outcome.unwrap_or_else(|e| format!("retry: {e}")))
    }
}

/// The Worker's D1 databases by its shard map (CF11, `shards`): every D1
/// access starts here. A mapping that does not parse, or names a binding
/// the Worker lacks, refuses (`d1.shards_invalid`, the binding's error).
pub(crate) fn env_shards(env: &Env) -> Result<crate::shards::D1Shards<WorkersD1>> {
    let value = env.var(crate::shards::VAR).ok().map(|v| v.to_string());
    let map = crate::shards::ShardMap::parse(value.as_deref())
        .map_err(|code| worker::Error::from(code.to_owned()))?;
    crate::shards::D1Shards::open(map, |name| env.d1(name).map(WorkersD1::new))
}

// ---------------------------------------------------------------------------
// Cron and the queue.

const CRON_REDELIVERY: &str = "* * * * *";
const CRON_ARCHIVES: &str = "17 0 * * *";

/// The Cron Trigger (CF4, CF3, CF5): every minute, webhook redelivery over
/// D1, admission and supersede notices still undelivered, half-committed
/// starts and unfinished run purges; after midnight UTC, the archive and
/// retention job over D1, R2 and the objects (`archives::CfArchiveHost`).
#[event(scheduled)]
async fn scheduled(event: worker::ScheduledEvent, env: Env, _ctx: worker::ScheduleContext) {
    let cron = event.cron();
    // Gate fix B1: as `fetch`, nothing runs while a test seam is set.
    if let Some(name) = crate::seams::set_in(&env) {
        worker::console_error!("config.test_seam_set: {name}");
        return;
    }
    let outcome = if cron == CRON_ARCHIVES {
        let _ = ensure_schema(&env).await;
        match WorkersObjects::remote(&env).and_then(|objects| WorkersHost::new(&env, objects)) {
            Ok(host) => {
                let backends = CfBackends::new(host);
                // CF12b: replay responses kept in R2 whose rows expired.
                // Review round 1: a failure is logged and reported, not
                // read as nothing to sweep.
                let replays = match crate::cron::replay_pass(&backends, js_sys::Date::now() as i64)
                    .await
                {
                    Ok(swept) => {
                        if swept.failed > 0 {
                            worker::console_error!(
                                "cron.replay_sweep: {} object deletes failed; their rows are kept",
                                swept.failed
                            );
                        }
                        json!({ "rows": swept.rows, "failed": swept.failed })
                    }
                    Err(error) => {
                        worker::console_error!("cron.replay_sweep: {}", code(&error));
                        json!({ "error": code(&error) })
                    }
                };
                let host = crate::archives::CfArchiveHost::new(backends);
                let quotas = quota_sweep_pass(&env).await;
                // CF11: delivery-log retention, as the native drain's.
                let delivery_log = match (env_shards(&env), crate::notify::webhooks(&env)) {
                    (Ok(shards), Some(webhooks)) => {
                        crate::cron::delivery_log_pass(&shards, &webhooks)
                            .await
                            .unwrap_or(0)
                    }
                    _ => 0,
                };
                match crate::cron::archive_pass(&host).await {
                    Ok(pass) => json!({
                        "cron": cron, "tenants": pass.tenants, "quotas": quotas,
                        "delivery_log_purged": delivery_log,
                        "replays_swept": replays,
                        "written": pass.job.written, "removed": pass.job.removed,
                        "live_deleted": pass.job.live_deleted,
                        "archives_expired": pass.job.archives_expired,
                        "summaries_purged": pass.job.summaries_purged,
                        "versions_swept": pass.job.versions_swept,
                        // LANG4: children kept until their parent has the
                        // result (cancel the parent to let them go).
                        "call_results_pending": pass.job.results_pending,
                        "call_cancels_pending": pass.job.cancels_pending,
                        "failed": pass.failed.len(),
                    }),
                    Err(error) => json!({ "cron": cron, "error": code(&error) }),
                }
            }
            Err(error) => json!({ "cron": cron, "error": format!("binding: {error}") }),
        }
    } else if cron != CRON_REDELIVERY {
        json!({ "cron": cron, "error": "cron.unknown" })
    } else {
        let _ = ensure_schema(&env).await;
        // Project events fan out to their subscriptions' WebhookDOs, which
        // attempt and retry them on their own alarms (CF11).
        let project_events = match (env_shards(&env), crate::notify::webhooks(&env)) {
            (Ok(shards), Some(webhooks)) => crate::cron::project_events_pass(&shards, &webhooks)
                .await
                .map_or_else(|error| json!({ "error": code(&error) }), |n| json!(n)),
            _ => json!("unconfigured"),
        };
        // Each shard's notices and unsettled starts (CF11).
        let (mut notices, mut reconciled) = (0, 0);
        if let Ok(shards) = env_shards(&env) {
            for (binding, db) in shards.data_shards() {
                notices += deliver_notices_pass(&env, &db).await;
                reconciled += reconcile_pass(&env, &shards, &binding, &db).await;
            }
        }
        let (supersedes, purges) = objects_pass(&env).await;
        // LANG9b: schedules due this minute.
        let schedules = schedule_pass(&env).await;
        // NOTIFY1: digests and due mail through the auth Worker's transport.
        let email = match (
            env_shards(&env),
            crate::auth::WorkersSessions::from_env(&env, &origin(&env)),
        ) {
            (Ok(shards), Some(sessions)) => {
                let transport = crate::notify::AuthMail(sessions);
                match crate::cron::email_pass(&shards.control(), &transport, &origin(&env)).await {
                    Ok((sent, digests)) => json!({ "sent": sent, "digests": digests }),
                    Err(error) => json!({ "error": code(&error) }),
                }
            }
            _ => json!("unconfigured"),
        };
        json!({
            "cron": cron, "project_events": project_events,
            "admission_notices": notices, "reconciled_starts": reconciled,
            "supersede_notices": supersedes, "purges": purges,
            "schedules": schedules, "email": email,
        })
    };
    worker::console_log!("cron {outcome}");
}

/// Pages of admission notices one minute pass tries (review fix 6: a page
/// of notices that keep failing never hides the ones after it).
const NOTICE_PAGES: usize = 10;
const NOTICE_PAGE: usize = 50;

/// Deliver undelivered admission notices, oldest first, paging past each
/// attempted one. Returns how many ended (applied or terminal).
async fn deliver_notices_pass(env: &Env, db: &WorkersD1) -> usize {
    let mut ended = 0;
    let mut after = None;
    for _ in 0..NOTICE_PAGES {
        let Ok(page) = runbook_store_d1::admission::pending_notices_after(
            db,
            None,
            after.as_ref(),
            NOTICE_PAGE,
        )
        .await
        else {
            break;
        };
        for notice in &page {
            if deliver_notice(env, db, notice).await {
                ended += 1;
            }
        }
        match page.last() {
            Some(last) if page.len() == NOTICE_PAGE => {
                after = Some((last.admitted_at_ms, last.execution_id.clone()));
            }
            _ => break,
        }
    }
    ended
}

/// CF6: judge every organization's old quota reservations against D1
/// (`quota::sweep`): starts that never committed are refunded, counted runs
/// forgotten. Returns (refunded, settled) over all organizations.
async fn quota_sweep_pass(env: &Env) -> serde_json::Value {
    use runbook_store_d1::sql::{Columns, prepare};
    let Ok(objects) = WorkersObjects::remote(env) else {
        return json!("unconfigured");
    };
    let Ok(shards) = env_shards(env) else {
        return json!("unconfigured");
    };
    // The organizations are the directory's (control); each one's
    // reservations are judged against its own shard (CF11).
    let Ok(orgs) = runbook_store_d1::D1::all(
        &shards.control(),
        prepare("SELECT org_id FROM organization"),
    )
    .await
    else {
        return json!({ "error": "d1.organizations" });
    };
    let before = js_sys::Date::now() as i64 - crate::quota::SWEEP_GRACE_MS;
    let (mut refunded, mut settled, mut failed) = (0, 0, 0);
    for row in &orgs {
        let Ok(org) = row.parse::<runbook_model::OrganizationId>(0) else {
            continue;
        };
        match crate::quota::sweep(&shards.for_org(&org), &objects.quota(&org), before, 200).await {
            Ok(swept) => {
                refunded += swept.refunded;
                settled += swept.settled;
            }
            Err(_) => failed += 1,
        }
    }
    json!({ "refunded": refunded, "settled": settled, "failed": failed })
}

/// How long a supersede notice or a purge may stay before the cron takes
/// it over from the host that recorded it (that host normally finishes it
/// within its request).
const OBJECT_WORK_GRACE_MS: i64 = 60_000;

/// CF5: LANG3 supersede notices and run purges a host did not finish,
/// through the object protocol. Returns how many of each ended.
async fn objects_pass(env: &Env) -> (usize, usize) {
    let Ok(host) = WorkersObjects::remote(env).and_then(|objects| WorkersHost::new(env, objects))
    else {
        return (0, 0);
    };
    let backends = CfBackends::new(host);
    let shards = backends.host().shards();
    let before = js_sys::Date::now() as i64 - OBJECT_WORK_GRACE_MS;
    let (mut supersedes, mut purges) = (0, 0);
    // Each shard's own organizations' work (CF11).
    for (binding, db) in shards.data_shards() {
        let (s, p) = objects_pass_on(&backends, &shards, &binding, &db, before).await;
        supersedes += s;
        purges += p;
    }
    // Gate fix D2: runs an import committed but did not release. Gate round
    // 2: the rows are the control database's (they commit with an import's
    // control batch), and one that fails backs off.
    let control = shards.control();
    if let Ok(due) = runbook_store_d1::portable::pending_releases(&control, before, 25).await {
        for (scope, id) in due {
            if crate::portable::release_run(&backends.execution(scope), &id)
                .await
                .is_err()
            {
                let _ = runbook_store_d1::portable::defer_release(
                    &control,
                    &id,
                    js_sys::Date::now() as i64,
                )
                .await;
            }
        }
    }
    (supersedes, purges)
}

async fn objects_pass_on(
    backends: &CfBackends<WorkersHost>,
    shards: &crate::shards::D1Shards<WorkersD1>,
    binding: &str,
    db: &WorkersD1,
    before: i64,
) -> (usize, usize) {
    let mut supersedes = 0;
    if let Ok(due) = runbook_store_d1::start::pending_supersedes(db, before, 25).await {
        for notice in due {
            if !shards.owns(binding, &notice.scope.org_id) {
                continue;
            }
            let store = backends.execution(notice.scope.clone());
            if store
                .deliver_supersede(&notice.run, &notice.superseded)
                .await
                .is_ok()
            {
                supersedes += 1;
            } else {
                // Gate fix (low): it waits its turn again; the next ones go.
                let _ = runbook_store_d1::start::defer_supersede(
                    db,
                    &notice.run.execution_id,
                    js_sys::Date::now() as i64,
                )
                .await;
            }
        }
    }
    let mut purges = 0;
    if let Ok(due) = runbook_store_d1::retention::pending_purges(db, before, 25).await {
        for purge in due {
            if !shards.owns(binding, &purge.scope.org_id) {
                continue;
            }
            let store = backends.execution(purge.scope.clone());
            if store
                .finish_purge(&purge.execution_id, purge.keep_uploads)
                .await
                .is_ok()
            {
                purges += 1;
            } else {
                // Gate fix (low): it waits its turn again; the next ones go.
                let _ = runbook_store_d1::retention::defer_purge(
                    db,
                    &purge.execution_id,
                    js_sys::Date::now() as i64,
                )
                .await;
            }
        }
    }
    (supersedes, purges)
}

/// Review fix 2: send each start that never reached D1 (past the grace
/// period) to its object to be finished or abandoned.
async fn reconcile_pass(
    env: &Env,
    shards: &crate::shards::D1Shards<WorkersD1>,
    binding: &str,
    db: &WorkersD1,
) -> usize {
    let before = js_sys::Date::now() as i64 - runbook_store_d1::start::START_GRACE_MS;
    let mut done = 0;
    // Gate fix (low): a few pages a pass, past the starts already tried.
    let mut cursor: Option<(i64, ExecutionId)> = None;
    for _ in 0..RECONCILE_PAGES {
        let Ok(page) = runbook_store_d1::start::unsettled_starts_after(
            db,
            before,
            cursor.as_ref().map(|(at, id)| (*at, id)),
            RECONCILE_PAGE,
        )
        .await
        else {
            break;
        };
        let full = page.len() == RECONCILE_PAGE;
        cursor = page.last().map(|(_, id, at)| (*at, id.clone()));
        for (scope, id, _) in page {
            if reconcile_one(env, shards, binding, &scope, &id).await {
                done += 1;
            }
        }
        if !full {
            break;
        }
    }
    done
}

const RECONCILE_PAGES: usize = 4;
const RECONCILE_PAGE: usize = 25;

/// Send one unsettled start to its object (finished or abandoned there).
async fn reconcile_one(
    env: &Env,
    shards: &crate::shards::D1Shards<WorkersD1>,
    binding: &str,
    scope: &TenantScope,
    id: &ExecutionId,
) -> bool {
    if !shards.owns(binding, &scope.org_id) {
        return false;
    }
    let Ok(namespace) = env.durable_object("EXECUTIONS") else {
        return false;
    };
    let Ok(stub) = namespace
        .id_from_name(id.as_str())
        .and_then(|object| object.get_stub())
    else {
        return false;
    };
    let mut init = worker::RequestInit::new();
    init.with_method(Method::Post);
    let Ok(request) = Request::new_with_init(
        &format!("https://do/{id}/reconcile?org={}", scope.org_id.as_str()),
        &init,
    ) else {
        return false;
    };
    matches!(
        futures_util::future::select(
            Box::pin(stub.fetch_with_request(request)),
            Box::pin(worker::Delay::from(std::time::Duration::from_millis(
                NOTICE_TIMEOUT_MS,
            ))),
        )
        .await,
        futures_util::future::Either::Left((Ok(ref response), _)) if response.status_code() == 200
    )
}

/// The notification queue's consumer (CF4, over D1 since CF3).
#[event(queue)]
async fn queue(
    batch: worker::MessageBatch<crate::delivery::NotificationMessage>,
    env: Env,
    _ctx: worker::Context,
) -> Result<()> {
    // Gate fix B1: an error retries the batch once the seam is unset.
    if let Some(name) = crate::seams::set_in(&env) {
        return Err(worker::Error::from(format!("config.test_seam_set: {name}")));
    }
    ensure_schema(&env).await?;
    crate::notify::consume_batch(batch, &env).await
}
