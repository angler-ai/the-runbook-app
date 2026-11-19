//! CF0: the native backends of the portable router (`runbook_api::Router`):
//! Postgres stores, OS entropy, the blocking image pipeline and the
//! in-memory rate limiter.
use runbook_api::backends::{Backends, IssueOutcome, Limited, NoEntropy, TokenIssue};
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    credentials::{CredentialError, CredentialRecord, SecretHash, issuance::TokenMetadata},
    device_codes::DeviceCodeRecord,
    directory::DirectoryError,
    inbox::{InboxFilters, MeTasksPage, MeTasksScope},
    uploads::{ImagePipeline, ProcessedImage},
};
use runbook_model::{
    ExecutionId, MemoryId, OrganizationId, ProjectId, RunbookId, StepRunId, UserId,
};
use runbook_store_postgres::{
    DatabaseConfig, PostgresAuthoring, PostgresCredentials, PostgresDirectory,
    PostgresExecutionStore, PostgresWebhooks,
};
use std::sync::Arc;

/// One image through the pipeline at a time per server (UP2 fixes): a
/// finalize that would wait for it answers 503 `server.busy` instead of
/// holding a request slot in a queue.
pub(crate) type ImageSlot = Arc<tokio::sync::Semaphore>;

#[derive(Clone)]
pub struct PostgresBackends {
    pub(crate) config: DatabaseConfig,
    pub(crate) webhooks: crate::config::WebhookConfig,
    pub(crate) retention: crate::config::RetentionConfig,
    pub(crate) image_caps: crate::images::Caps,
    pub(crate) image_slot: ImageSlot,
    /// AUTH2: the auth child, when sign-in is configured.
    pub(crate) auth: Option<Arc<crate::session::Sessions>>,
    /// HOST1: this instance's windows, used only while Postgres is
    /// unreachable.
    pub(crate) local_limits: Arc<crate::device::Limits>,
    /// HOST1 review: keys the shared windows refused, answered from memory.
    pub(crate) refused: Arc<crate::device::Refused>,
    /// STO1: object storage handle when GCS or S3 is configured.
    pub(crate) objects: Option<Arc<runbook_store_objects::ObjectStorage>>,
    /// NOTIFY1: the app's public origin, for links in email.
    pub(crate) origin: String,
    /// SLACK1 stage 2: the operator's Slack app.
    pub(crate) slack: Option<runbook_core::slack::SlackApp>,
}

impl PostgresBackends {
    pub fn new(
        server: &crate::config::ServerConfig,
        image_slot: ImageSlot,
        auth: Option<Arc<crate::session::Sessions>>,
    ) -> Self {
        let objects = server.blob_store.build_storage().ok().flatten();
        Self {
            config: server.database.clone(),
            webhooks: server.webhooks.clone(),
            retention: server.retention.clone(),
            image_caps: server.uploads.image_caps(),
            image_slot,
            auth,
            local_limits: Arc::default(),
            refused: Arc::default(),
            objects,
            origin: server.origin.clone(),
            slack: server.slack.clone(),
        }
    }
}

/// HOST1 review: the longest a rate-limited request waits for a database
/// connection before it is refused (503), instead of the pool's 30 s.
pub const LIMIT_ACQUIRE: std::time::Duration = std::time::Duration::from_secs(2);

/// CF0: the router's configuration from the server's.
pub fn router_config(server: &crate::config::ServerConfig) -> runbook_api::router::RouterConfig {
    runbook_api::router::RouterConfig {
        discovery: runbook_api::discovery::DiscoveryConfig {
            origin: server.origin.clone(),
            minimum_cli_version: "0.1.0".into(),
            max_request_bytes: server.max_request_bytes as u64,
            version: runbook_api::VERSION.into(),
            schema: [("postgres".to_owned(), u64::from(server.schema_version))].into(),
            theme: server.theme.clone(),
            step_runs: server.database.step_cap(),
        },
        authoring: server.authoring.clone(),
        uploads: server.uploads.settings(&server.origin),
        upload_limits: server.uploads.advertised_limits(),
        sign_in: server
            .auth
            .as_ref()
            .map(|auth| runbook_api::router::SignIn {
                allowed_domains: auth.allowed_domains.clone(),
            }),
    }
}

fn issue_outcome(outcome: runbook_store_postgres::IssueOutcome) -> IssueOutcome {
    match outcome {
        runbook_store_postgres::IssueOutcome::Issued => IssueOutcome::Issued,
        runbook_store_postgres::IssueOutcome::AlreadyIssued => IssueOutcome::AlreadyIssued,
        runbook_store_postgres::IssueOutcome::KeyReused => IssueOutcome::KeyReused,
    }
}

/// D51 on this host: [`crate::images::process`] on a blocking thread. The
/// caller holds the image slot, so one image is decoded at a time.
pub struct NativeImages {
    caps: crate::images::Caps,
}
impl ImagePipeline for NativeImages {
    async fn process(
        &self,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<Option<ProcessedImage>, &'static str> {
        let content_type = content_type.to_owned();
        let caps = self.caps;
        // A decoder panic is a file we cannot read, never a server crash.
        let image = tokio::task::spawn_blocking(move || {
            crate::images::process(&bytes, &content_type, caps)
        })
        .await
        .map_err(|_| "upload.image_invalid")??;
        Ok(Some(ProcessedImage {
            bytes: image.bytes,
            content_type: image.content_type.into(),
            width: image.width,
            height: image.height,
            thumbnail: image.thumbnail,
        }))
    }
}

impl Backends for PostgresBackends {
    type Directory = PostgresDirectory;
    type Credentials = PostgresCredentials;
    type Execution = PostgresExecutionStore;
    type Webhooks = PostgresWebhooks;
    type Images = NativeImages;
    type ImagePermit = tokio::sync::OwnedSemaphorePermit;
    type WebhookHost = crate::webhooks::ServerHost;
    type Executions = crate::executions::PostgresExecutionBackend;
    type Authoring = PostgresAuthoring;
    type Archives = crate::daily_archives::PostgresArchiveBackend;
    type Notify = runbook_store_postgres::PostgresNotify;

    fn directory(&self) -> PostgresDirectory {
        PostgresDirectory::new(self.config.clone())
    }
    fn credentials(&self) -> PostgresCredentials {
        PostgresCredentials::new(self.config.clone())
    }
    fn execution(&self, scope: TenantScope) -> PostgresExecutionStore {
        PostgresExecutionStore::new(self.config.clone(), scope).with_objects(self.objects.clone())
    }
    fn webhooks(&self) -> PostgresWebhooks {
        PostgresWebhooks::new(self.config.clone())
    }
    fn notify(&self) -> runbook_store_postgres::PostgresNotify {
        runbook_store_postgres::PostgresNotify::new(self.config.clone())
    }
    fn origin(&self) -> String {
        self.origin.clone()
    }
    fn executions(&self) -> crate::executions::PostgresExecutionBackend {
        crate::executions::PostgresExecutionBackend {
            config: self.config.clone(),
            objects: self.objects.clone(),
        }
    }
    fn authoring(&self) -> PostgresAuthoring {
        PostgresAuthoring::new(self.config.clone())
    }
    async fn published_versions(
        &self,
    ) -> Result<Vec<runbook_core::triggers::PublishedVersion>, runbook_core::StoreError> {
        runbook_store_postgres::published_versions(&self.config).await
    }
    fn archives(&self) -> crate::daily_archives::PostgresArchiveBackend {
        crate::daily_archives::PostgresArchiveBackend {
            config: self.config.clone(),
            retention: self.retention.clone(),
            objects: self.objects.clone(),
        }
    }
    fn auth_methods(&self) -> Vec<String> {
        self.auth
            .as_ref()
            .map(|auth| auth.methods())
            .unwrap_or_default()
    }
    fn webhook_host(&self) -> crate::webhooks::ServerHost {
        crate::webhooks::ServerHost::new(&self.webhooks)
    }
    fn images(&self) -> NativeImages {
        NativeImages {
            caps: self.image_caps,
        }
    }
    fn try_image_slot(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.image_slot.clone().try_acquire_owned().ok()
    }
    async fn oauth_apps(
        &self,
        route: &'static str,
        body: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, runbook_api::session::Unavailable> {
        match &self.auth {
            Some(sessions) => sessions.oauth_apps(route, &body).await,
            None => Ok(None),
        }
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
    ) -> Result<serde_json::Value, String> {
        use runbook_core::slack::SlackTransport;
        crate::slack_http::SlackHttp
            .slack_api(token, method, query, body)
            .await
    }
    async fn slack_respond(&self, url: &str, body: &serde_json::Value) -> Result<(), String> {
        crate::slack_http::SlackHttp.respond(url, body).await
    }
    fn wall_clock_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(-1)
    }
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), NoEntropy> {
        getrandom::fill(bytes).map_err(|_| NoEntropy)
    }
    /// HOST1: the windows live in Postgres, so every replica on the
    /// database counts against one limit. HOST1 review: a key already
    /// known refused is refused from memory; a busy pool or a failing query
    /// fails closed (`Limited::Unavailable`, 503) within
    /// [`LIMIT_ACQUIRE`]; only an unreachable database falls back to this
    /// instance's own windows.
    async fn rate_limit(&self, key: &str, window_ms: u64, limit: u32) -> Result<(), Limited> {
        use runbook_store_postgres::RateLimitError;
        if let Some(retry) = self.refused.check(key) {
            return Err(Limited::Retry(retry));
        }
        match runbook_store_postgres::rate_limit_hit(
            &self.config,
            key,
            window_ms,
            limit,
            LIMIT_ACQUIRE,
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(retry)) => {
                self.refused.remember(key, retry);
                Err(Limited::Retry(retry))
            }
            Err(RateLimitError::Unreachable) => self
                .local_limits
                .hit(key, std::time::Duration::from_millis(window_ms), limit)
                .map_err(Limited::Retry),
            Err(RateLimitError::Busy | RateLimitError::Failed) => Err(Limited::Unavailable),
        }
    }
    async fn execution_owner(
        &self,
        org: &OrganizationId,
        execution: &ExecutionId,
    ) -> Result<Option<TenantScope>, StoreError> {
        Ok(
            runbook_store_postgres::execution_owner(&self.config, execution)
                .await?
                .filter(|scope| &scope.org_id == org),
        )
    }
    async fn execution_scope(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<TenantScope>, CredentialError> {
        self.credentials().execution_scope(hash).await
    }
    async fn token_issue_replay(
        &self,
        org: &OrganizationId,
        principal: &str,
        key: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<IssueOutcome>, CredentialError> {
        Ok(self
            .credentials()
            .issue_replay(org, principal, key, payload)
            .await?
            .map(issue_outcome))
    }
    async fn token_issue(&self, issue: &TokenIssue) -> Result<IssueOutcome, CredentialError> {
        let issue = runbook_store_postgres::TokenIssue {
            record: issue.record.clone(),
            metadata: issue.metadata.clone(),
            principal: issue.principal.clone(),
            key: issue.key.clone(),
            payload: issue.payload.clone(),
            response: issue.response.clone(),
        };
        Ok(issue_outcome(self.credentials().issue(&issue).await?))
    }
    async fn token_insert_issued(
        &self,
        record: &CredentialRecord,
        metadata: &TokenMetadata,
    ) -> Result<(), CredentialError> {
        self.credentials().insert_issued(record, metadata).await
    }
    async fn tokens_of_org(
        &self,
        org: &OrganizationId,
    ) -> Result<Vec<(CredentialRecord, TokenMetadata)>, CredentialError> {
        self.credentials().list_org(org).await
    }
    async fn device_code_by_device_hash(
        &self,
        device_code_hash: &SecretHash,
    ) -> Result<Option<DeviceCodeRecord>, CredentialError> {
        self.credentials()
            .device_code_by_device_hash(device_code_hash)
            .await
    }
    async fn active_orgs_of_user(
        &self,
        user: &UserId,
    ) -> Result<Vec<OrganizationId>, DirectoryError> {
        self.directory().active_orgs_of_user(user).await
    }
    async fn expired_step_run(
        &self,
        org: &OrganizationId,
        step_run: &StepRunId,
    ) -> Result<Option<(ExecutionId, ProjectId)>, StoreError> {
        runbook_api::backends::org_store(self, org)
            .expired_step_run(org, step_run)
            .await
    }
    async fn memory_project(
        &self,
        org: &OrganizationId,
        id: &MemoryId,
    ) -> Result<Option<ProjectId>, StoreError> {
        runbook_api::backends::org_store(self, org)
            .memory_project(org, id)
            .await
    }
    async fn runbook_project(
        &self,
        org: &OrganizationId,
        id: &RunbookId,
    ) -> Result<Option<ProjectId>, StoreError> {
        runbook_api::backends::org_store(self, org)
            .runbook_project(org, id)
            .await
    }
    async fn me_tasks(
        &self,
        scope: &MeTasksScope,
        filters: &InboxFilters,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MeTasksPage, StoreError> {
        PostgresAuthoring::new(self.config.clone())
            .me_tasks(scope, filters, cursor, limit)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// CF0 review 3: the router's future for one request stays small, so a
    /// deep poll chain (an `/mcp` message calling the router) fits the
    /// worker's stack, and wasm's. Each area's future is boxed in
    /// `Router::dispatch`; this is the state machine left on the stack.
    #[test]
    fn the_router_future_is_small() {
        let server = crate::config::ServerConfig::new(
            DatabaseConfig::from_values(Some("postgres://unused@localhost/unused".into()), None)
                .unwrap(),
            "https://runbooks.example",
            "127.0.0.1:1",
        )
        .unwrap();
        let router = runbook_api::router::Router::new(
            PostgresBackends::new(&server, Arc::new(tokio::sync::Semaphore::new(1)), None),
            router_config(&server),
        );
        let request = runbook_api::Request {
            method: "GET".into(),
            path: "/v1/me".into(),
            query: Default::default(),
            body: serde_json::Value::Null,
            credential: None,
            idempotency_key: None,
            request_id: "req_test".into(),
            new_id: "00000000000000000000000001".into(),
            if_match: None,
            client_ip: None,
        };
        let size = std::mem::size_of_val(&router.handle(request));
        println!("router future: {size} bytes");
        // 43,432 bytes in a debug build before the boxing; 632 after.
        assert!(size <= 1024, "the router future is {size} bytes");
    }
}
