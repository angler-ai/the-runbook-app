/// UPD1: the release version, one number for every binary and both editions:
/// the workspace version (`[workspace.package] version` in Cargo.toml).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub mod backends;
pub mod device;
pub mod directory;
pub mod discovery;
pub mod evidence;
pub mod execution_access;
pub mod grants;
pub mod http;
pub mod invitations;
pub mod mcp;
pub mod me;
pub mod me_tasks;
pub mod memory;
pub mod mfa;
pub mod oauth;
pub mod pictures;
pub mod problem;
pub mod router;
pub mod routes;
pub mod runner_access;
mod runner_context;
pub mod runner_proxy;
pub mod runners;
pub mod session;
pub mod slack;
pub mod slack_app;
pub mod sse;
pub mod step_reads;
pub mod tokens;
pub mod universal;
pub mod web;
pub use directory::Service;
/// Owned transport data; intentionally no Debug implementation for bearer secrets.
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: std::collections::BTreeMap<String, String>,
    pub body: serde_json::Value,
    /// How the caller authenticated, if at all (D46: a bearer header wins
    /// over a cookie, so a request carries exactly one).
    pub credential: Option<Credential>,
    pub idempotency_key: Option<String>,
    pub request_id: String,
    pub new_id: String,
    pub if_match: Option<String>,
    /// The client's address as the host determined it (natively the TCP
    /// peer, or the client a trusted proxy named; on Cloudflare
    /// `cf-connecting-ip`), when known. Recorded on device codes.
    pub client_ip: Option<String>,
}
pub mod variables;

pub mod authoring;

pub mod executions;
pub mod field_caps;
pub mod search;
pub mod triggers;

pub mod actor_requests;

pub mod actor_store;
pub mod actors;
pub mod execution_credentials;

pub mod webhooks;

pub mod planned;

pub mod archives;

/// The presented credential: a bearer header, or (AUTH2) a session cookie
/// the auth service vouched for (D44). No Debug implementation: a bearer
/// value is a secret.
pub enum Credential {
    /// The `Authorization: Bearer` secret (`trb_pat_`, `trb_svc_`, `trb_exe_` …).
    Bearer(String),
    /// A bearer looked up once by the router and shared with its handler.
    ResolvedBearer(String, Box<runbook_core::credentials::CredentialRecord>),
    /// A browser session the auth service vouched for, or (MCP2) an OAuth
    /// access token it introspected (`scopes` set): `/mcp` builds that one
    /// for its tools' in-process calls; it never comes from outside.
    Session(ResolvedSession),
}

/// What `POST /internal/session` returns for a live session cookie (D44).
/// The host caches it for 30 s, keyed by the cookie's SHA-256.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedSession {
    /// Better Auth's user ID, mapped to a directory user by `AuthLinkStore`.
    pub auth_user_id: String,
    pub email: String,
    pub email_verified: bool,
    /// The person's name from their sign-in (Google's), if any: the
    /// account's first name when this session creates it.
    pub name: Option<String>,
    /// Their Google photo (`https://`), if any: their profile photo until
    /// they upload one.
    pub picture_url: Option<String>,
    pub session_id: String,
    pub expires_at_ms: i64,
    pub mfa_verified_at: Option<i64>,
    pub mfa_method: Option<session::MfaMethod>,
    pub mfa_enrolled: Option<bool>,
    /// The directory principal the host linked this session to (D45), as
    /// `runbook_core::auth_links::session_credential` builds it from the
    /// directory for this request. `None` until linked: an unlinked session
    /// authenticates nothing.
    pub principal: Option<Box<runbook_core::credentials::CredentialRecord>>,
    /// MCP2: an introspected OAuth access token's granted scopes (the
    /// answer's `scope`, space separated); `None` for a browser session.
    /// [`session::link`] then builds an agent principal with these scopes,
    /// narrowed to the user's own.
    pub scopes: Option<String>,
}

/// Why a presented credential resolves to no record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialFailure {
    /// 401 `auth.required`: nothing presented.
    Required,
    /// 401 `auth.invalid_credential`: unknown, malformed or unlinked.
    Invalid,
    /// 503 `storage.unavailable`.
    Unavailable,
}
impl CredentialFailure {
    pub fn status(self) -> (u16, &'static str) {
        match self {
            Self::Required => (401, "auth.required"),
            Self::Invalid => (401, "auth.invalid_credential"),
            Self::Unavailable => (503, "storage.unavailable"),
        }
    }
}

impl Request {
    /// The bearer secret, if the caller presented one. Session callers have
    /// none, so every bearer-only route treats them as unauthenticated.
    pub fn bearer(&self) -> Option<&str> {
        match &self.credential {
            Some(Credential::Bearer(secret) | Credential::ResolvedBearer(secret, _)) => {
                Some(secret)
            }
            Some(Credential::Session(_)) | None => None,
        }
    }
    /// True when the caller authenticated with a session cookie, or (MCP2)
    /// is an OAuth grant riding the session credential. Gates that refuse
    /// sessions use this.
    pub fn is_session(&self) -> bool {
        matches!(self.credential, Some(Credential::Session(_)))
    }
    /// True only for a browser session cookie: never an OAuth grant (MCP2
    /// re-check 5). Gates that admit a person in a browser use this.
    pub fn is_browser_session(&self) -> bool {
        matches!(&self.credential, Some(Credential::Session(s)) if s.scopes.is_none())
    }
    /// The credential record the caller authenticates as: a stored PAT or
    /// service token found by its secret's hash (of the kind its prefix
    /// names), or a linked session's principal. Validity (expiry,
    /// revocation) and authorization stay with the caller, as before.
    /// Execution and refresh secrets are not in this store: `Invalid`.
    pub async fn credential_record<C: runbook_core::credentials::CredentialStore>(
        &self,
        store: &C,
    ) -> Result<runbook_core::credentials::CredentialRecord, CredentialFailure> {
        use runbook_core::credentials::{parse_secret, secret_hash};
        match &self.credential {
            None => Err(CredentialFailure::Required),
            Some(Credential::Bearer(secret)) => {
                let (kind, _) = parse_secret(secret).map_err(|_| CredentialFailure::Invalid)?;
                store
                    .find_by_hash(&secret_hash(secret))
                    .await
                    .map_err(|_| CredentialFailure::Unavailable)?
                    .filter(|record| record.kind == kind)
                    .ok_or(CredentialFailure::Invalid)
            }
            Some(Credential::ResolvedBearer(_, record)) => Ok(record.as_ref().clone()),
            Some(Credential::Session(session)) => session
                .principal
                .as_deref()
                .cloned()
                .ok_or(CredentialFailure::Invalid),
        }
    }
}
