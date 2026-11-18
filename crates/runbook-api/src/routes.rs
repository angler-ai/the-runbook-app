//! CF0: every route this API serves, with who answers it. The router and
//! the hosts must agree with this list, and it with the contract
//! (`api/openapi.draft.json`): `tests/route_manifest.rs` checks both ways.
//!
//! Execution credentials (D32) add no routes: their allow-list
//! ([`crate::execution_access`]) answers a subset of the paths below.
use crate::router::Area;

/// Who answers a route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Served {
    /// [`crate::router::Router::handle`], in this area.
    Router(Area),
    /// The host, before or beside the router.
    Host(Host),
}

/// Routes a host answers itself (the transport and its streams).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// SSE plumbing (`text/event-stream`); natively the worker's stream hub,
    /// on Workers the ExecutionDO bridge (D58).
    EventStream,
    /// MCP1 Streamable HTTP: [`crate::mcp`] with the host's limits.
    Mcp,
    /// AUTH2 (D43): the auth service's own routes, proxied.
    AuthProxy,
    /// D50: signed uploads and reads, on the user-content origin only.
    UserContent,
}

/// How a route relates to the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// In `api/openapi.draft.json`.
    Public,
    /// Never public: reachable only from the host's own services
    /// (`/internal/*`: loopback plus the per-boot secret natively, a service
    /// binding on Workers).
    Internal,
    /// Served like its replacement but absent from the contract (F56).
    Deprecated,
    /// A public endpoint outside the REST contract (its own protocol or
    /// origin).
    Transport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub method: &'static str,
    /// The path pattern; `{name}` is one segment, `{*rest}` the rest.
    pub path: &'static str,
    pub served: Served,
    pub kind: Kind,
}

const fn api(method: &'static str, path: &'static str, area: Area, kind: Kind) -> Route {
    Route {
        method,
        path,
        served: Served::Router(area),
        kind,
    }
}
const fn host(method: &'static str, path: &'static str, host: Host, kind: Kind) -> Route {
    Route {
        method,
        path,
        served: Served::Host(host),
        kind,
    }
}

/// Every route, method by method.
pub const MANIFEST: &[Route] = &[
    api("GET", "/.well-known/runbook", Area::Directory, Kind::Public),
    api("POST", "/v1/device/lookup", Area::Device, Kind::Public),
    api("POST", "/v1/device/approve", Area::Device, Kind::Public),
    api("POST", "/v1/device/code", Area::Device, Kind::Public),
    api("POST", "/v1/device/token", Area::Device, Kind::Public),
    api(
        "GET",
        "/v1/executions/{exe}",
        Area::Executions,
        Kind::Public,
    ),
    api("PATCH", "/v1/executions/{exe}", Area::Actors, Kind::Public),
    api(
        "GET",
        "/v1/executions/{exe}/archive",
        Area::Archives,
        Kind::Public,
    ),
    api(
        "DELETE",
        "/v1/executions/{exe}/credentials/{family}",
        Area::ExecutionCredentials,
        Kind::Public,
    ),
    host(
        "GET",
        "/v1/executions/{exe}/events",
        Host::EventStream,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/executions/{exe}/evidence",
        Area::Evidence,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/executions/{exe}/next",
        Area::StepReads,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/executions/{exe}/report",
        Area::Executions,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/executions/{exe}/rerun",
        Area::Executions,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/executions/{exe}/signals/{name}",
        Area::Actors,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/executions/{exe}/tokens",
        Area::ExecutionCredentials,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/executions/{exe}/tokens/refresh",
        Area::ExecutionCredentials,
        Kind::Public,
    ),
    api("GET", "/v1/me", Area::Me, Kind::Public),
    api("PATCH", "/v1/me", Area::Me, Kind::Public),
    api("GET", "/v1/me/tasks", Area::MeTasks, Kind::Public),
    api(
        "GET",
        "/v1/orgs/{org}/search",
        Area::UniversalSearch,
        Kind::Public,
    ),
    api("GET", "/v1/projects/{p}/slack", Area::Slack, Kind::Public),
    api("PUT", "/v1/me/picture", Area::Pictures, Kind::Public),
    api("DELETE", "/v1/me/picture", Area::Pictures, Kind::Public),
    api(
        "GET",
        "/v1/users/{user}/picture",
        Area::Pictures,
        Kind::Public,
    ),
    api("PATCH", "/v1/projects/{p}", Area::Pictures, Kind::Public),
    api(
        "GET",
        "/v1/projects/{p}/picture",
        Area::Pictures,
        Kind::Public,
    ),
    api(
        "PUT",
        "/v1/projects/{p}/picture",
        Area::Pictures,
        Kind::Public,
    ),
    api(
        "DELETE",
        "/v1/projects/{p}/picture",
        Area::Pictures,
        Kind::Public,
    ),
    api("PUT", "/v1/projects/{p}/slack", Area::Slack, Kind::Public),
    api(
        "DELETE",
        "/v1/projects/{p}/slack",
        Area::Slack,
        Kind::Public,
    ),
    api("DELETE", "/v1/me/token", Area::Me, Kind::Public),
    api("GET", "/v1/me/email-preferences", Area::Me, Kind::Public),
    api("PUT", "/v1/me/email-preferences", Area::Me, Kind::Public),
    api("POST", "/v1/unsubscribe", Area::Me, Kind::Public),
    api("GET", "/v1/me/apps", Area::Me, Kind::Public),
    api("DELETE", "/v1/me/apps/{client}", Area::Me, Kind::Public),
    api("GET", "/v1/memory/{mem}", Area::Memory, Kind::Public),
    api("PATCH", "/v1/memory/{mem}", Area::Memory, Kind::Public),
    api(
        "GET",
        "/v1/memory/{mem}/revisions",
        Area::Memory,
        Kind::Public,
    ),
    api("GET", "/v1/meta", Area::Directory, Kind::Public),
    api("GET", "/v1/orgs", Area::Directory, Kind::Public),
    api(
        "GET",
        "/v1/orgs/{org}/invitations",
        Area::Invitations,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/orgs/{org}/invitations",
        Area::Invitations,
        Kind::Public,
    ),
    api(
        "DELETE",
        "/v1/orgs/{org}/invitations/{inv}",
        Area::Invitations,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/orgs/{org}/projects",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/orgs/{org}/projects",
        Area::Directory,
        Kind::Public,
    ),
    api("GET", "/v1/orgs/{org}/teams", Area::Directory, Kind::Public),
    api(
        "POST",
        "/v1/orgs/{org}/teams",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/orgs/{org}/teams/{team}/members",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/orgs/{org}/teams/{team}/members",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "DELETE",
        "/v1/orgs/{org}/teams/{team}/members/{user}",
        Area::Directory,
        Kind::Public,
    ),
    api("GET", "/v1/orgs/{org}/users", Area::Directory, Kind::Public),
    api(
        "POST",
        "/v1/orgs/{org}/users",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "PATCH",
        "/v1/orgs/{org}/users/{user}",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/executions",
        Area::Executions,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/projects/{p}/executions",
        Area::Executions,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/projects/{p}/runbooks/{rb}/triggers/{trigger}",
        Area::Executions,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/members",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/projects/{p}/members",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "DELETE",
        "/v1/projects/{p}/members/{user}",
        Area::Directory,
        Kind::Public,
    ),
    api("GET", "/v1/projects/{p}/memory", Area::Memory, Kind::Public),
    api(
        "POST",
        "/v1/projects/{p}/memory",
        Area::Memory,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/projects/{p}/sync",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/runbooks",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/projects/{p}/runbooks",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/runbooks/{slug}",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/skills",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/projects/{p}/skills",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/skills/{hash}",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "DELETE",
        "/v1/projects/{p}/vars",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/vars",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "PUT",
        "/v1/projects/{p}/vars",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "DELETE",
        "/v1/projects/{p}/vars/{name}",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/vars/{name}",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "PUT",
        "/v1/projects/{p}/vars/{name}",
        Area::Directory,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/webhooks",
        Area::Webhooks,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/projects/{p}/webhooks",
        Area::Webhooks,
        Kind::Public,
    ),
    api(
        "DELETE",
        "/v1/projects/{p}/webhooks/{id}",
        Area::Webhooks,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/webhooks/{id}",
        Area::Webhooks,
        Kind::Public,
    ),
    api(
        "PATCH",
        "/v1/projects/{p}/webhooks/{id}",
        Area::Webhooks,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/projects/{p}/webhooks/{id}/deliveries",
        Area::Webhooks,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/projects/{p}/webhooks/{id}/deliveries/{d}/redeliver",
        Area::Webhooks,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/projects/{p}/webhooks/{id}/rotate-secret",
        Area::Webhooks,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/projects/{p}/webhooks/{id}/test",
        Area::Webhooks,
        Kind::Public,
    ),
    api("GET", "/v1/runbooks/{rb}", Area::Authoring, Kind::Public),
    api(
        "GET",
        "/v1/runbooks/{rb}/archives",
        Area::Archives,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/runbooks/{rb}/archives/{date}",
        Area::Archives,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/runbooks/{rb}/draft",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "PATCH",
        "/v1/runbooks/{rb}/draft",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/runbooks/{rb}/memory",
        Area::Memory,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/runbooks/{rb}/memory",
        Area::Memory,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/runbooks/{rb}/versions",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/runbooks/{rb}/versions",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/runbooks/{rb}/versions/{a}/diff/{b}",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/runbooks/{rb}/versions/{version}",
        Area::Authoring,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/runbooks/{rb}/versions/{version}/form",
        Area::Authoring,
        Kind::Public,
    ),
    api("GET", "/v1/step-runs/{sr}", Area::StepReads, Kind::Public),
    api(
        "POST",
        "/v1/step-runs/{sr}/checklist",
        Area::Actors,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/step-runs/{sr}/claim",
        Area::Actors,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/step-runs/{sr}/decisions",
        Area::Actors,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/step-runs/{sr}/evidence",
        Area::Evidence,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/step-runs/{sr}/evidence/{evd}/finalize",
        Area::Evidence,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/step-runs/{sr}/heartbeat",
        Area::Actors,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/step-runs/{sr}/logs",
        Area::StepReads,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/step-runs/{sr}/logs",
        Area::Actors,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/step-runs/{sr}/overrides",
        Area::Actors,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/step-runs/{sr}/release",
        Area::Actors,
        Kind::Public,
    ),
    api(
        "POST",
        "/v1/step-runs/{sr}/report",
        Area::Actors,
        Kind::Public,
    ),
    api(
        "GET",
        "/v1/step-runs/{sr}/skill-files",
        Area::StepReads,
        Kind::Public,
    ),
    api("GET", "/v1/tokens", Area::Tokens, Kind::Public),
    api("POST", "/v1/tokens", Area::Tokens, Kind::Public),
    api("DELETE", "/v1/tokens/{id}", Area::Tokens, Kind::Public),
    api("POST", "/v1/validate", Area::Authoring, Kind::Public),
    api(
        "POST",
        "/v1/step-runs/{sr}/decision",
        Area::Actors,
        Kind::Deprecated,
    ),
    api(
        "POST",
        "/internal/can-register",
        Area::CanRegister,
        Kind::Internal,
    ),
    host("POST", "/mcp", Host::Mcp, Kind::Transport),
    // Every method: the auth service decides.
    host("*", "/api/auth/{*rest}", Host::AuthProxy, Kind::Transport),
    // MCP2: the OAuth discovery documents (`runbook_api::oauth::PROXIED_PATHS`),
    // answered by the auth service: RFC 9728 for `/mcp`, RFC 8414 for it.
    host(
        "GET",
        "/.well-known/oauth-protected-resource",
        Host::AuthProxy,
        Kind::Transport,
    ),
    host(
        "GET",
        "/.well-known/oauth-protected-resource/mcp",
        Host::AuthProxy,
        Kind::Transport,
    ),
    host(
        "GET",
        "/.well-known/oauth-authorization-server",
        Host::AuthProxy,
        Kind::Transport,
    ),
    host(
        "GET",
        "/.well-known/oauth-authorization-server/api/auth",
        Host::AuthProxy,
        Kind::Transport,
    ),
    host("PUT", "/u/{exe}/{evd}", Host::UserContent, Kind::Transport),
    host(
        "OPTIONS",
        "/u/{exe}/{evd}",
        Host::UserContent,
        Kind::Transport,
    ),
    host("GET", "/u/{exe}/{evd}", Host::UserContent, Kind::Transport),
    host("HEAD", "/u/{exe}/{evd}", Host::UserContent, Kind::Transport),
    host(
        "GET",
        "/u/{exe}/{evd}/thumbnail",
        Host::UserContent,
        Kind::Transport,
    ),
    host(
        "HEAD",
        "/u/{exe}/{evd}/thumbnail",
        Host::UserContent,
        Kind::Transport,
    ),
];
