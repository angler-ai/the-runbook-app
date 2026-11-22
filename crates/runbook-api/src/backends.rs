//! CF0: the backends the portable router runs on.
//!
//! One bundle, implemented once per host: Postgres plus native helpers in
//! `runbook-server`, D1/Durable Objects/R2 bindings on Workers. Every future
//! is `?Send` (no `Send` bound anywhere), so a Workers `fetch` handler can
//! drive the same router a native single-threaded executor drives.
//!
//! The store traits come from `runbook-core`. The few lookups no core trait
//! covers yet (token issuance, the execution an execution secret belongs to,
//! a user's organizations, …) are methods here, so adding them to a host is
//! one place, and nothing in `runbook-core` changes shape for CF0.
use runbook_core::{
    StoreError,
    auth_links::AuthLinkStore,
    blob::BlobStore,
    catalog::{CatalogStore, TenantScope},
    claims::ClaimStore,
    credentials::{
        CredentialError, CredentialRecord, CredentialStore, SecretHash, issuance::TokenMetadata,
    },
    device_codes::{DeviceCodeRecord, DeviceCodeStore},
    directory::{DirectoryError, DirectoryStore},
    directory_operations::DirectoryOperations,
    execution_credentials::ExecutionCredentialStore,
    inbox::{InboxFilters, MeTasksPage, MeTasksScope},
    memory_entries::MemoryStore,
    session::SessionStore,
    start::StartStore,
    step_lookup::StepRunLookup,
    uploads::{ImagePipeline, UploadStore},
    webhooks::WebhookStore,
};
use runbook_model::{
    ExecutionId, MemoryId, OrganizationId, ProjectId, RunbookId, StepRunId, UserId,
};
use std::future::Future;

/// Token issuance's D20 outcome (B4.1/C2): what a create under an
/// idempotency key did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueOutcome {
    Issued,
    /// Same principal, key and body: the secret was already shown once.
    AlreadyIssued,
    /// Same principal and key, different body.
    KeyReused,
}

/// One token to store with its D20 identity and saved response.
#[derive(Clone, Debug)]
pub struct TokenIssue {
    pub record: CredentialRecord,
    pub metadata: TokenMetadata,
    pub principal: String,
    pub key: String,
    pub payload: serde_json::Value,
    pub response: serde_json::Value,
}

/// The host could not supply randomness (`503 server.entropy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoEntropy;

/// Why [`Backends::rate_limit`] refused a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limited {
    /// The limit is reached; retry after this many seconds (`429`).
    Retry(u64),
    /// The shared windows cannot be read right now (HOST1 review: a busy
    /// pool or a failing database); refuse rather than count nothing
    /// (`503`).
    Unavailable,
}
impl Limited {
    /// The response for a refused request.
    pub fn response(self, id: &str) -> crate::problem::Response {
        match self {
            Self::Retry(retry) => {
                let mut response = crate::problem::Response::error(429, "limit.rate", id);
                response.body["retry_after"] = serde_json::json!(retry);
                response
            }
            Self::Unavailable => crate::problem::Response::error(503, "storage.unavailable", id),
        }
    }
}

/// Everything the router needs from its host.
pub trait Backends: Clone {
    type Directory: DirectoryStore
        + DirectoryOperations
        + AuthLinkStore
        + runbook_core::pictures::PictureStore;
    type Credentials: CredentialStore + DeviceCodeStore;
    /// One tenant's execution-side stores.
    type Execution: StartStore
        + BlobStore
        + SessionStore
        + CatalogStore
        + ClaimStore
        + StepRunLookup
        + MemoryStore
        + UploadStore
        + ExecutionCredentialStore;
    type Webhooks: WebhookStore;
    /// NOTIFY1: email preferences and the outbox.
    type Notify: runbook_core::notify::NotifyStore;
    /// D51: decode and re-encode images (a blocking pool natively).
    type Images: ImagePipeline;
    /// Held while one image is in the pipeline.
    type ImagePermit;
    /// C1 host capabilities (sealer, entropy, DNS vetting).
    type WebhookHost: crate::webhooks::WebhookHost;
    /// B5 starts, reads, search and reports.
    type Executions: crate::executions::ExecutionBackend;
    /// B4.3 authoring and catalog stores.
    type Authoring: runbook_core::authoring::AuthoringFactory;
    /// ARC daily archives.
    type Archives: crate::archives::ArchiveBackend;
    /// R0: the runner registry, agent directory and grants
    /// (`runbook_core::runners::Unsupported` on a host without them).
    type Runners: runbook_core::runners::RunnerStore + runbook_core::agent_directory::AgentDirectory;

    fn directory(&self) -> Self::Directory;
    fn credentials(&self) -> Self::Credentials;
    /// The stores of one tenant. Org-keyed lookups that need no project
    /// use [`zero_project`] as the project.
    fn execution(&self, scope: TenantScope) -> Self::Execution;
    fn webhooks(&self) -> Self::Webhooks;
    fn notify(&self) -> Self::Notify;
    /// NOTIFY1: the app's public origin, for links in email. Empty when
    /// the host does not know it (links stay relative).
    fn origin(&self) -> String {
        String::new()
    }
    fn executions(&self) -> Self::Executions;
    fn authoring(&self) -> Self::Authoring;
    fn archives(&self) -> Self::Archives;
    fn runners(&self) -> Self::Runners;
    /// R0-2: runner URLs may be private addresses
    /// (`RUNBOOK_RUNNER_PRIVATE_NETWORKS=true`).
    fn runner_private_networks(&self) -> bool {
        false
    }
    /// LANG9b: every non-archived runbook's latest published version, for
    /// the schedule pass ([`crate::triggers::schedule_pass`]).
    fn published_versions(
        &self,
    ) -> impl Future<Output = Result<Vec<runbook_core::triggers::PublishedVersion>, StoreError>>;
    /// AUTH2: the sign-in methods discovery lists (what the auth service
    /// reported, or its configuration until it has).
    fn auth_methods(&self) -> Vec<String>;
    fn webhook_host(&self) -> Self::WebhookHost;
    fn images(&self) -> Self::Images;
    /// UP2: one image in the pipeline at a time; `None` when it is busy.
    fn try_image_slot(&self) -> Option<Self::ImagePermit>;
    /// MCP2: one of the auth service's internal connected-app routes
    /// (`oauth/grants`, `oauth/grants/revoke`) with a JSON body. `Ok(None)`
    /// when the host has no auth service.
    fn auth_internal(
        &self,
        route: &'static str,
        body: serde_json::Value,
    ) -> impl Future<Output = Result<Option<serde_json::Value>, crate::session::Unavailable>> {
        let _ = (route, body);
        std::future::ready(Ok(None))
    }

    /// R0-1: the keys this host signs runner grants and user assertions
    /// with (`signing.unavailable` when it has none).
    fn signing_keys(
        &self,
    ) -> impl Future<Output = Result<runbook_core::signing::KeyRing, runbook_core::signing::SigningError>>
    {
        std::future::ready(Err(runbook_core::signing::SigningError::Unavailable))
    }
    /// R0-1: the published keys (`GET /.well-known/jwks.json`): current and
    /// recently retired. Empty when the host signs nothing.
    fn jwks(
        &self,
    ) -> impl Future<Output = Result<runbook_core::signing::Jwks, runbook_core::signing::SigningError>>
    {
        std::future::ready(Ok(runbook_core::signing::Jwks::default()))
    }

    /// R0-5: one request to a runner (the host vets the address like a
    /// webhook target, follows no redirect, bounds size and time). An
    /// error is a stable code: `runner.timeout`, `runner.unreachable`,
    /// `runner.url_forbidden`, `runner.bad_response`, `runner.unsupported`.
    fn runner_fetch(
        &self,
        request: crate::runner_proxy::RunnerHttpRequest,
    ) -> impl Future<Output = Result<crate::runner_proxy::RunnerHttpResponse, &'static str>> {
        let _ = request;
        std::future::ready(Err("runner.unsupported"))
    }

    /// The host's wall clock, in epoch milliseconds (request nonces; the
    /// stores keep their own clock for every decision).
    fn wall_clock_ms(&self) -> i64;
    /// SLACK1 stage 2: the operator's Slack app, when configured.
    fn slack_app(&self) -> Option<runbook_core::slack::SlackApp> {
        None
    }
    /// SLACK1 stage 2: a Slack Web API call ([`runbook_core::slack::SlackTransport::slack_api`]).
    fn slack_api(
        &self,
        token: &str,
        method: &str,
        query: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> impl Future<Output = Result<serde_json::Value, String>> {
        let _ = (token, method, query, body);
        async { Err("slack.unavailable".to_owned()) }
    }
    /// SLACK1 stage 2: a JSON POST to an interaction's `response_url`
    /// (always `https://hooks.slack.com/…`, checked by the caller).
    fn slack_respond(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> impl Future<Output = Result<(), String>> {
        let _ = (url, body);
        async { Err("slack.unavailable".to_owned()) }
    }
    /// Fill `bytes` from the host's cryptographic RNG.
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), NoEntropy>;
    /// AUTH3 sliding-window limit: count one hit for `key`, or
    /// `Err(Limited::Retry(seconds))` once `limit` hits were already counted
    /// in the last `window_ms`; `Err(Limited::Unavailable)` when the windows
    /// cannot be read (the caller answers 503, never "allowed").
    /// Gate fix B2: async, so a host can keep the windows somewhere every
    /// instance shares (the Cloudflare edition's LimitsDO).
    fn rate_limit(
        &self,
        key: &str,
        window_ms: u64,
        limit: u32,
    ) -> impl Future<Output = Result<(), Limited>>;

    // ---- lookups no core trait covers yet ----

    /// The tenant an execution belongs to, if it is `org`'s; `None` for
    /// another organization's run (CF11: the lookup is org-scoped, so a
    /// sharded edition reads only `org`'s database).
    fn execution_owner(
        &self,
        org: &OrganizationId,
        execution: &ExecutionId,
    ) -> impl Future<Output = Result<Option<TenantScope>, StoreError>>;
    /// The tenant of the run an execution or refresh secret belongs to.
    fn execution_scope(
        &self,
        hash: &SecretHash,
    ) -> impl Future<Output = Result<Option<TenantScope>, CredentialError>>;
    /// D20 probe before validating a retried token create.
    fn token_issue_replay(
        &self,
        org: &OrganizationId,
        principal: &str,
        key: &str,
        payload: &serde_json::Value,
    ) -> impl Future<Output = Result<Option<IssueOutcome>, CredentialError>>;
    fn token_issue(
        &self,
        issue: &TokenIssue,
    ) -> impl Future<Output = Result<IssueOutcome, CredentialError>>;
    /// Store a minted token with its metadata (device login).
    fn token_insert_issued(
        &self,
        record: &CredentialRecord,
        metadata: &TokenMetadata,
    ) -> impl Future<Output = Result<(), CredentialError>>;
    fn token_metadata(
        &self,
        org: &OrganizationId,
        id: &runbook_model::TokenId,
    ) -> impl Future<Output = Result<TokenMetadata, CredentialError>>;
    fn tokens_of_org(
        &self,
        org: &OrganizationId,
    ) -> impl Future<Output = Result<Vec<(CredentialRecord, TokenMetadata)>, CredentialError>>;
    fn device_code_by_device_hash(
        &self,
        device_code_hash: &SecretHash,
    ) -> impl Future<Output = Result<Option<DeviceCodeRecord>, CredentialError>>;
    /// The organizations where a user is an active member.
    fn active_orgs_of_user(
        &self,
        user: &UserId,
    ) -> impl Future<Output = Result<Vec<OrganizationId>, DirectoryError>>;
    /// A step run whose claim lease expired (step reads explain the 409).
    fn expired_step_run(
        &self,
        org: &OrganizationId,
        step_run: &StepRunId,
    ) -> impl Future<Output = Result<Option<(ExecutionId, ProjectId)>, StoreError>>;
    fn memory_project(
        &self,
        org: &OrganizationId,
        id: &MemoryId,
    ) -> impl Future<Output = Result<Option<ProjectId>, StoreError>>;
    fn runbook_project(
        &self,
        org: &OrganizationId,
        id: &RunbookId,
    ) -> impl Future<Output = Result<Option<ProjectId>, StoreError>>;
    /// B1 `GET /v1/me/tasks`.
    fn me_tasks(
        &self,
        scope: &MeTasksScope,
        filters: &InboxFilters,
        cursor: Option<&str>,
        limit: usize,
    ) -> impl Future<Output = Result<MeTasksPage, StoreError>>;
}

/// The sentinel project of org-keyed lookups; never queried as a project.
pub fn zero_project() -> ProjectId {
    "prj_00000000000000000000000000"
        .parse()
        .expect("zero project ID")
}

/// The org-keyed store of `org` (lookups that take the org explicitly).
pub fn org_store<B: Backends>(backends: &B, org: &OrganizationId) -> B::Execution {
    backends.execution(TenantScope {
        org_id: org.clone(),
        project_id: zero_project(),
    })
}

/// A new sortable ID: `prefix` plus 26 Crockford base32 characters, the
/// 48-bit millisecond time then 80 random bits (as `runbook-server`'s
/// bootstrap IDs).
pub fn new_id<B: Backends>(backends: &B, prefix: &str, now_ms: i64) -> Result<String, NoEntropy> {
    let mut random = [0u8; 10];
    backends.fill_random(&mut random)?;
    encode_id(prefix, now_ms, random).ok_or(NoEntropy)
}

/// The ID encoding shared by every host: `None` when the time is out of
/// the 48-bit range.
pub fn encode_id(prefix: &str, now_ms: i64, random: [u8; 10]) -> Option<String> {
    if !(0..=0xffff_ffff_ffff).contains(&now_ms) {
        return None;
    }
    let mut value = (now_ms as u128) << 80;
    for (i, b) in random.into_iter().enumerate() {
        value |= u128::from(b) << ((9 - i) * 8);
    }
    let mut bytes = [b'0'; 26];
    for b in bytes.iter_mut().rev() {
        *b = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[(value & 31) as usize];
        value >>= 5;
    }
    Some(format!("{prefix}{}", std::str::from_utf8(&bytes).ok()?))
}
