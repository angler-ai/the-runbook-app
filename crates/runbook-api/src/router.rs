//! CF0: the API router. One dispatch order for every host: the native
//! server's worker thread and a Workers `fetch` handler call
//! [`Router::handle`] with an owned [`Request`] and get a [`Response`].
//!
//! The host decides beforehand what never reaches a router: transport
//! limits, `/internal/*` callers (loopback with the per-boot secret on the
//! native server, a service binding on Workers), the auth service proxy,
//! event streams and the user-content origin.
use crate::{
    Credential, Request,
    authoring::{AuthoringLimits, AuthoringService},
    backends::Backends,
    discovery::DiscoveryConfig,
    evidence::UploadSettings,
    problem::Response,
};
use runbook_model::UploadLimits;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

/// AUTH2: sign-in is configured (device login, the registration hook and
/// discovery's sign-in methods exist).
#[derive(Clone, Debug, Default)]
pub struct SignIn {
    /// D45 registration domains; empty allows any.
    pub allowed_domains: Vec<String>,
}

/// What the router needs from the host's configuration.
#[derive(Clone)]
pub struct RouterConfig {
    pub discovery: DiscoveryConfig,
    pub authoring: AuthoringLimits,
    pub uploads: UploadSettings,
    /// The upload limits `/v1/meta` advertises (the pipeline's caps
    /// included, so clients shrink first).
    pub upload_limits: UploadLimits,
    pub sign_in: Option<SignIn>,
}

pub struct Router<B> {
    pub backends: B,
    pub config: RouterConfig,
}

impl<B: Backends> Router<B> {
    pub fn new(backends: B, config: RouterConfig) -> Self {
        Self { backends, config }
    }

    /// AUTH2 `POST /internal/can-register`, for a host that has already
    /// established the caller is its auth service (natively loopback plus
    /// the per-boot secret, on Workers a service binding). Without sign-in
    /// it does not exist.
    pub async fn can_register(&self, request: Request) -> Response {
        match &self.config.sign_in {
            Some(sign_in) if request.path == crate::session::CAN_REGISTER => {
                crate::session::can_register(&self.backends, &sign_in.allowed_domains, request)
                    .await
            }
            _ => Response::error(404, "resource.not_found", &request.request_id),
        }
    }

    /// Answer one API request.
    pub async fn handle(&self, mut request: Request) -> Response {
        // `/internal/*` is the host's own services' only, through their own
        // entry points ([`Router::can_register`]); a host that forwards
        // every path still exposes none of it.
        if request.path.starts_with("/internal/") {
            return Response::error(404, "resource.not_found", &request.request_id);
        }
        // Later-plans rule 5: a planned operation answers 501 before any
        // authentication. The native host also checks before reading the
        // body; this keeps every host's answer the same.
        if crate::planned::recognizes(&request.method, &request.path) {
            let (status, code) = crate::planned::NOT_IMPLEMENTED;
            return Response::error(status, code, &request.request_id);
        }
        // F45: a body from a caller with no credential is refused before
        // any route (except the refresh route and the device grant, whose
        // bodies are the credential). The native host decides it from the
        // headers before reading the body; a host without that check gets
        // the same answer here.
        if request.credential.is_none()
            && !request.body.is_null()
            && crate::http::anonymous_body_refused(&request.method, &request.path)
        {
            return Response::error(401, "auth.required", &request.request_id);
        }
        let is_meta = request.method == "GET" && request.path == "/v1/meta";
        let is_discovery = request.method == "GET" && request.path == "/.well-known/runbook";
        // D45/D46: link a session on first use and build its principal.
        if let Some(Credential::Session(session)) = request.credential.as_mut()
            && let Err((status, code)) =
                Box::pin(crate::session::link(&self.backends, session)).await
        {
            return Response::error(status, code, &request.request_id);
        }
        let mut response = self.dispatch(request).await;
        if response.status == 200 {
            if is_discovery {
                self.enrich_discovery(&mut response.body);
            }
            if is_meta {
                self.enrich_meta(&mut response.body);
            }
        }
        response
    }

    /// Each area's future is boxed: their state machines differ in size by
    /// tens of kilobytes, and an unboxed `match` would make every request's
    /// future as large as the largest (CF0 review 3: the worker's stack, and
    /// wasm's, are small).
    fn dispatch(&self, r: Request) -> Pin<Box<dyn Future<Output = Response> + '_>> {
        let b = &self.backends;
        let execution = crate::execution_access::is_execution(&r);
        match area(&r.method, &r.path, execution) {
            // Only [`Router::can_register`] answers it; `handle` never gets here.
            Area::CanRegister => Box::pin(std::future::ready(Response::error(
                404,
                "resource.not_found",
                &r.request_id,
            ))),
            Area::Device => match &self.config.sign_in {
                Some(_) => Box::pin(crate::device::handle(b, &self.config.discovery.origin, r)),
                None => Box::pin(std::future::ready(Response::error(
                    404,
                    "resource.not_found",
                    &r.request_id,
                ))),
            },
            Area::Invitations => Box::pin(crate::invitations::handle(b, r)),
            Area::Memory => Box::pin(crate::memory::handle(b, r)),
            Area::Archives => Box::pin(async move {
                crate::archives::ArchiveService {
                    directory: b.directory(),
                    credentials: b.credentials(),
                    backend: b.archives(),
                }
                .handle(r)
                .await
            }),
            Area::Evidence => Box::pin(crate::evidence::handle(b, &self.config.uploads, r)),
            Area::ExecutionCredentials => Box::pin(crate::execution_credentials::handle(b, r)),
            Area::ExecutionAccess => Box::pin(crate::execution_access::handle(b, r)),
            Area::Tokens => Box::pin(crate::tokens::handle(b, r)),
            Area::UniversalSearch => Box::pin(crate::universal::handle(b, r)),
            Area::Slack => Box::pin(crate::slack::handle(b, r)),
            Area::SlackInteractions => Box::pin(crate::slack_app::handle(b, r)),
            Area::Pictures => Box::pin(crate::pictures::handle(b, r)),
            Area::Me => Box::pin(crate::me::handle(b, r)),
            Area::MeTasks => Box::pin(crate::me_tasks::handle(b, r)),
            Area::StepReads => Box::pin(crate::step_reads::handle(b, r)),
            Area::Webhooks => Box::pin(async move {
                crate::webhooks::WebhookService {
                    directory: b.directory(),
                    credentials: b.credentials(),
                    store: b.webhooks(),
                    host: b.webhook_host(),
                }
                .handle(r)
                .await
            }),
            Area::Actors => Box::pin(async move {
                crate::actors::ActorService {
                    directory: b.directory(),
                    credentials: b.credentials(),
                    backend: crate::actor_store::StoreActorBackend {
                        backends: b.clone(),
                    },
                    evidence_schemes: vec![],
                }
                .handle(r)
                .await
            }),
            Area::Executions => Box::pin(async move {
                crate::executions::ExecutionService {
                    directory: b.directory(),
                    credentials: b.credentials(),
                    backend: b.executions(),
                    origin: self.config.discovery.origin.clone(),
                }
                .handle(r)
                .await
            }),
            Area::Authoring => Box::pin(async move {
                AuthoringService::new(
                    b.directory(),
                    b.credentials(),
                    b.authoring(),
                    self.config.authoring.clone(),
                )
                .handle(r)
                .await
            }),
            Area::Directory => Box::pin(async move {
                crate::Service::new(
                    b.directory(),
                    b.credentials(),
                    self.config.discovery.clone(),
                )
                .handle(r)
                .await
            }),
        }
    }

    /// AUTH2/AUTH3: the sign-in methods and the device endpoint.
    fn enrich_discovery(&self, body: &mut Value) {
        if self.config.sign_in.is_some() {
            body["auth"]["methods"] = json!(self.backends.auth_methods());
            body["auth"]["device_authorization_endpoint"] = json!(format!(
                "{}{}",
                self.config.discovery.origin,
                crate::device::CODE
            ));
        }
    }

    /// What this deployment serves and its limits.
    fn enrich_meta(&self, body: &mut Value) {
        let limits = &self.config.authoring;
        body["capabilities"]["executions"] = json!(true);
        body["capabilities"]["authoring"] = json!(true);
        body["capabilities"]["skills"] = json!(true);
        body["capabilities"]["mcp"] = json!(true);
        body["limits"]["source_bytes"] = json!(limits.source_bytes);
        body["limits"]["skill_bundle_bytes"] = if limits.skill_bytes == u64::MAX {
            Value::Null
        } else {
            json!(limits.skill_bytes)
        };
        body["capabilities"]["uploads"] = json!(self.config.uploads.enabled);
        body["limits"]["uploads"] =
            serde_json::to_value(&self.config.upload_limits).unwrap_or_default();
    }
}

/// Which part of the API answers a request. Decided from the method, the
/// path and whether the bearer is an execution credential (D32: those never
/// reach the person-facing handlers of a path they share), in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Area {
    /// AUTH2 `POST /internal/can-register` (the host gates who reaches it).
    CanRegister,
    Device,
    Invitations,
    Memory,
    Archives,
    Evidence,
    ExecutionCredentials,
    /// D32: an execution credential's step-at-a-time allow-list.
    ExecutionAccess,
    Tokens,
    Me,
    Slack,
    /// SLACK1 stage 2: Slack's signed button clicks.
    SlackInteractions,
    Pictures,
    UniversalSearch,
    MeTasks,
    StepReads,
    Webhooks,
    Actors,
    Executions,
    Authoring,
    /// The directory service: discovery, meta, orgs, projects, variables,
    /// validation, and the 404 of every unknown path.
    Directory,
}

pub fn area(method: &str, path: &str, execution_bearer: bool) -> Area {
    if path == crate::session::CAN_REGISTER {
        Area::CanRegister
    } else if crate::device::recognizes(method, path) {
        Area::Device
    } else if crate::invitations::recognizes(path) {
        Area::Invitations
    } else if crate::memory::recognizes(path) {
        Area::Memory
    } else if crate::archives::recognizes(path) {
        Area::Archives
    } else if crate::evidence::recognizes(path) {
        Area::Evidence
    } else if crate::execution_credentials::recognizes(path) {
        Area::ExecutionCredentials
    } else if execution_bearer {
        Area::ExecutionAccess
    } else if crate::tokens::recognizes(path) {
        Area::Tokens
    } else if crate::universal::recognizes(path) {
        Area::UniversalSearch
    } else if crate::slack_app::recognizes(path) {
        Area::SlackInteractions
    } else if crate::slack::recognizes(path) {
        Area::Slack
    } else if crate::pictures::recognizes(path) {
        Area::Pictures
    } else if crate::me::recognizes(path) {
        Area::Me
    } else if path == crate::me_tasks::PATH {
        Area::MeTasks
    } else if crate::step_reads::recognizes(path)
        || crate::step_reads::recognizes_logs(method, path)
    {
        Area::StepReads
    } else if crate::webhooks::recognizes(path) {
        Area::Webhooks
    } else if crate::actors::recognizes(method, path) {
        Area::Actors
    } else if crate::executions::recognizes(path) {
        Area::Executions
    } else if crate::authoring::recognizes(path) {
        Area::Authoring
    } else {
        Area::Directory
    }
}
