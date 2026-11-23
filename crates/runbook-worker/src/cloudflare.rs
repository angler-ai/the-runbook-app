//! CF3: `runbook_api::backends::Backends` for the Cloudflare edition, so
//! the api Worker and the ExecutionDO serve the real `runbook_api::Router`.
//!
//! Everything here is portable: the platform (bindings, entropy, clock,
//! webhook host) comes through [`CfHost`], implemented on wasm32 by the
//! edge (`edge.rs`) and natively by tests. The api Worker's host reaches no
//! ExecutionDO storage (its [`Objects`] is empty), so execution-scoped
//! requests are forwarded to the run's object, whose host reaches exactly
//! that object (CF2-5).
use crate::{
    compose::{CfStore, Objects},
    object::ObjectHandle,
};
use runbook_api::{
    backends::{Backends, IssueOutcome, NoEntropy, TokenIssue},
    executions::{
        ExecutionApiError as Error, ExecutionBackend, RerunInvocation, StartIdentity,
        StartInvocation, restore_state, store_error,
    },
};
use runbook_core::{
    ExecutionStore, StoreError, StoredExecution,
    authoring::{AuthoringCommand, AuthoringFactory, AuthoringStore, draft_response, skill_view},
    blob::{BlobKey, BlobStore},
    catalog::{
        Admission, AdmissionState, CatalogStore, RunbookRecord, SkillVersion, TenantScope,
        VersionRecord,
    },
    credentials::{CredentialError, CredentialRecord, SecretHash, issuance::TokenMetadata},
    device_codes::DeviceCodeRecord,
    directory::{DirectoryError, DirectoryStore},
    directory_operations::{ReplayResult, RequestReplay, StoredResponse},
    inbox::{InboxFilters, MeTasksPage, MeTasksScope},
    start::{
        StartCommand, StartError, StartOutcome, StartRecord, StartStore, commit_start_with_policy,
        prepare_start_commit,
    },
    uploads::{ImagePipeline, ProcessedImage},
};
use runbook_model::*;
use runbook_store_d1::{
    D1, D1Catalog, D1Credentials, D1Directory,
    sql::{Columns, guard, prepare},
};
use runbook_store_do::DoSql;
use runbook_store_r2::ObjectBucket;
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

/// What the platform gives the backends.
pub trait CfHost: Clone + 'static {
    type Sql: DoSql;
    type D1: D1;
    type Bucket: ObjectBucket;
    type Objects: Objects;
    type WebhookHost: runbook_api::webhooks::WebhookHost;
    /// Gate fix B2: where the AUTH3 rate-limit windows live.
    type Limits: crate::limits::RateLimits;
    fn limits(&self) -> Self::Limits;
    /// Every D1 database, by the shard map (CF11): `for_org` for an
    /// organization's data, `control` for the directory, credentials and
    /// token routes ([`crate::shards`]).
    fn shards(&self) -> crate::shards::D1Shards<Self::D1>;
    /// The blob bucket and the user-content bucket (D57).
    fn buckets(&self) -> (Self::Bucket, Self::Bucket);
    fn objects(&self) -> Self::Objects;
    fn webhook_host(&self) -> Self::WebhookHost;
    /// The host clock (request nonces; the stores keep their own).
    fn now_ms(&self) -> i64;
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), NoEntropy>;
    fn runner_fetch(
        &self,
        request: runbook_api::runner_proxy::RunnerHttpRequest,
    ) -> impl std::future::Future<
        Output = Result<runbook_api::runner_proxy::RunnerHttpResponse, &'static str>,
    > {
        let _ = request;
        std::future::ready(Err("runner.unsupported"))
    }
    /// AUTH2: the sign-in methods discovery lists (none without CF7).
    fn auth_methods(&self) -> Vec<String> {
        vec![]
    }
    /// NOTIFY1: the app's public origin, for links in email.
    fn public_origin(&self) -> String {
        String::new()
    }
    /// MCP2: the auth Worker's connected-app routes (none without CF7).
    fn auth_internal(
        &self,
        route: &'static str,
        body: serde_json::Value,
    ) -> impl std::future::Future<
        Output = Result<Option<serde_json::Value>, runbook_api::session::Unavailable>,
    > {
        let _ = (route, body);
        std::future::ready(Ok(None))
    }
    /// D54 retention (CF5): the defaults unless the host's vars set it.
    fn retention(&self) -> runbook_core::archives::job::RetentionPolicy {
        crate::archives::retention_policy(|_| None).unwrap_or_default()
    }
    /// CF6: the organization quota limits (`RUNBOOK_LIMIT_EXECUTIONS_MONTHLY`;
    /// off by default).
    fn quota_limits(&self) -> runbook_core::quotas::QuotaLimits {
        runbook_core::quotas::QuotaLimits::default()
    }
    /// CF13 review round 1: the run budget (`RUNBOOK_LIMIT_RUN_BYTES`;
    /// `runbook_core::run_budget::DEFAULT_MAX_RUN_BYTES` by default).
    fn run_budget(&self) -> u64 {
        runbook_core::run_budget::DEFAULT_MAX_RUN_BYTES
    }
    /// The per-run step cap (`RUNBOOK_LIMIT_STEP_RUNS`; none by default).
    fn step_cap(&self) -> Option<u64> {
        None
    }
    /// SLACK1 stage 2: the operator's Slack app (none by default).
    fn slack_app(&self) -> Option<runbook_core::slack::SlackApp> {
        None
    }
    /// SLACK1 stage 2: a Slack Web API call (unavailable by default).
    fn slack_api(
        &self,
        token: &str,
        method: &str,
        query: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, String>> {
        let _ = (token, method, query, body);
        async { Err("slack.unavailable".to_owned()) }
    }
    /// SLACK1 stage 2: a JSON POST to an interaction's `response_url`.
    fn slack_respond(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> impl std::future::Future<Output = Result<(), String>> {
        let _ = (url, body);
        async { Err("slack.unavailable".to_owned()) }
    }
    /// Test seam: how far ahead of the host clock the archive pass runs
    /// (`RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS`, as native); 0 otherwise.
    fn retention_clock_offset_ms(&self) -> i64 {
        0
    }
}

/// The Cloudflare bundle (the host keeps the rate-limit windows, B2).
#[derive(Clone)]
pub struct CfBackends<H: CfHost> {
    host: H,
}
impl<H: CfHost> CfBackends<H> {
    pub fn new(host: H) -> Self {
        Self { host }
    }
    pub fn host(&self) -> &H {
        &self.host
    }
    /// Retire the active signing key for this purpose. The old public key
    /// remains in the JWKS for the grant verification grace period.
    pub async fn rotate_signing_key(
        &self,
        purpose: runbook_core::signing::KeyPurpose,
    ) -> Result<(), runbook_core::signing::SigningError> {
        use runbook_api::webhooks::WebhookHost;
        let host = self.host.webhook_host();
        let sealer = host
            .sealer()
            .ok_or(runbook_core::signing::SigningError::Unavailable)?;
        let keys = runbook_store_d1::signing::D1SigningKeys::new(self.control());
        crate::signing::rotate(&keys, sealer, purpose, self.host.now_ms(), |bytes| {
            self.host.fill_random(bytes)
        })
        .await
    }
    fn catalog(&self, scope: TenantScope) -> D1Catalog<H::D1> {
        D1Catalog::new(self.host.shards().for_org(&scope.org_id), scope)
    }
    /// The control database (CF11): the directory, credentials, token
    /// routes and bootstrap.
    pub fn control(&self) -> H::D1 {
        self.host.shards().control()
    }
    /// CF12 review round 1: the directory over `db`, resolving request
    /// replay responses kept in a project's R2 blobs.
    fn replay_directory(&self, db: H::D1) -> D1Directory<H::D1> {
        D1Directory::new(db).with_replay_blobs(std::rc::Rc::new(CfReplayBlobs(self.clone())))
    }
    /// The database holding `org`'s data (CF11).
    pub fn for_org(&self, org: &OrganizationId) -> H::D1 {
        self.host.shards().for_org(org)
    }
}

pub type CfExecution<H> = CfStore<<H as CfHost>::D1, <H as CfHost>::Objects, <H as CfHost>::Bucket>;

/// The sentinel tenant of org-keyed lookups (`runbook_api::backends::zero_project`).
fn org_scope(org: &OrganizationId) -> TenantScope {
    TenantScope {
        org_id: org.clone(),
        project_id: runbook_api::backends::zero_project(),
    }
}

fn issue_outcome(outcome: runbook_store_d1::credentials::IssueOutcome) -> IssueOutcome {
    match outcome {
        runbook_store_d1::credentials::IssueOutcome::Issued => IssueOutcome::Issued,
        runbook_store_d1::credentials::IssueOutcome::AlreadyIssued => IssueOutcome::AlreadyIssued,
        runbook_store_d1::credentials::IssueOutcome::KeyReused => IssueOutcome::KeyReused,
    }
}

/// D51 on this edition: no decoder in the Worker yet, so images are kept
/// as uploaded (the pipeline answers "nothing to do").
pub struct NoImages;
impl ImagePipeline for NoImages {
    async fn process(
        &self,
        _bytes: Vec<u8>,
        _content_type: &str,
    ) -> Result<Option<ProcessedImage>, &'static str> {
        Ok(None)
    }
}

/// CF12 review round 1: a project's R2 blobs, where the authoring writer
/// keeps a replay response too large for its D1 row: under the project's
/// replay prefix since CF12b, in its content-addressed blobs before.
pub(crate) struct CfReplayBlobs<H: CfHost>(pub(crate) CfBackends<H>);
impl<H: CfHost> CfReplayBlobs<H> {
    fn store(&self, org: &OrganizationId, project: &runbook_model::ProjectId) -> CfExecution<H> {
        self.0.execution(TenantScope {
            org_id: org.clone(),
            project_id: project.clone(),
        })
    }
}
impl<H: CfHost> runbook_store_d1::replay::ReplayBlobs for CfReplayBlobs<H> {
    fn get(
        &self,
        org: &OrganizationId,
        project: &runbook_model::ProjectId,
        digest: [u8; 32],
    ) -> runbook_store_d1::sources::SourceFuture<'_, Option<Vec<u8>>> {
        let store = self.store(org, project);
        Box::pin(async move {
            match store.get(&BlobKey::Shared { hash: digest }).await {
                Ok(bytes) => Ok(Some(bytes)),
                Err(StoreError::NotFound) => Ok(None),
                Err(error) => Err(error),
            }
        })
    }
    fn get_reply(
        &self,
        org: &OrganizationId,
        project: &runbook_model::ProjectId,
        name: &str,
    ) -> runbook_store_d1::sources::SourceFuture<'_, Option<Vec<u8>>> {
        let store = self.store(org, project);
        let name = name.to_owned();
        Box::pin(async move { store.blobs().reply_get(&name).await })
    }
    fn delete_reply(
        &self,
        org: &OrganizationId,
        project: &runbook_model::ProjectId,
        name: &str,
    ) -> runbook_store_d1::sources::SourceFuture<'_, ()> {
        let store = self.store(org, project);
        let name = name.to_owned();
        Box::pin(async move { store.blobs().reply_delete(&name).await })
    }
}

impl<H: CfHost> Backends for CfBackends<H> {
    type Directory = D1Directory<H::D1>;
    type Credentials = D1Credentials<H::D1>;
    type Execution = CfExecution<H>;
    type Webhooks =
        crate::webhooks::CfWebhooks<H::D1, <H::Objects as crate::compose::Objects>::Webhooks>;
    type Images = NoImages;
    type ImagePermit = ();
    type WebhookHost = H::WebhookHost;
    type Executions = CfExecutions<H>;
    type Authoring = CfAuthoring<H>;
    type Archives = crate::archives::CfArchives<H>;
    type Runners = runbook_store_d1::runners::D1Runners<H::D1>;
    type Notify = runbook_store_d1::D1Notify<H::D1>;

    fn directory(&self) -> Self::Directory {
        self.replay_directory(self.control())
    }
    fn credentials(&self) -> Self::Credentials {
        D1Credentials::new(self.control())
    }
    fn execution(&self, scope: TenantScope) -> Self::Execution {
        let (blobs, user_content) = self.host.buckets();
        CfStore::sharded(
            self.host.shards(),
            self.host.objects(),
            scope,
            blobs,
            user_content,
        )
        .with_quota(self.host.quota_limits())
        .with_run_budget(self.host.run_budget())
    }
    fn webhooks(&self) -> Self::Webhooks {
        crate::webhooks::CfWebhooks::sharded(self.host.shards(), self.host.objects().webhooks())
    }
    fn notify(&self) -> Self::Notify {
        runbook_store_d1::D1Notify::new(self.control())
    }
    fn origin(&self) -> String {
        self.host.public_origin()
    }
    fn executions(&self) -> Self::Executions {
        CfExecutions {
            backends: self.clone(),
        }
    }
    fn authoring(&self) -> Self::Authoring {
        CfAuthoring {
            backends: self.clone(),
        }
    }
    fn runners(&self) -> Self::Runners {
        runbook_store_d1::runners::D1Runners::new(self.control())
    }
    async fn signing_keys(
        &self,
    ) -> Result<runbook_core::signing::KeyRing, runbook_core::signing::SigningError> {
        use runbook_api::webhooks::WebhookHost;
        let host = self.host.webhook_host();
        let sealer = host
            .sealer()
            .ok_or(runbook_core::signing::SigningError::Unavailable)?;
        let keys = runbook_store_d1::signing::D1SigningKeys::new(self.control());
        crate::signing::ring(&keys, sealer, self.host.now_ms(), |bytes| {
            self.host.fill_random(bytes)
        })
        .await
    }
    async fn jwks(
        &self,
    ) -> Result<runbook_core::signing::Jwks, runbook_core::signing::SigningError> {
        use runbook_api::webhooks::WebhookHost;
        let host = self.host.webhook_host();
        let Some(sealer) = host.sealer() else {
            return Ok(runbook_core::signing::Jwks { keys: Vec::new() });
        };
        let keys = runbook_store_d1::signing::D1SigningKeys::new(self.control());
        crate::signing::jwks(&keys, sealer, self.host.now_ms(), |bytes| {
            self.host.fill_random(bytes)
        })
        .await
    }
    async fn runner_fetch(
        &self,
        request: runbook_api::runner_proxy::RunnerHttpRequest,
    ) -> Result<runbook_api::runner_proxy::RunnerHttpResponse, &'static str> {
        self.host.runner_fetch(request).await
    }
    fn archives(&self) -> Self::Archives {
        crate::archives::CfArchives::new(self.clone())
    }
    async fn published_versions(
        &self,
    ) -> Result<Vec<runbook_core::triggers::PublishedVersion>, runbook_core::StoreError> {
        crate::archives::published_versions(self).await
    }
    fn auth_methods(&self) -> Vec<String> {
        self.host.auth_methods()
    }
    fn auth_internal(
        &self,
        route: &'static str,
        body: serde_json::Value,
    ) -> impl std::future::Future<
        Output = Result<Option<serde_json::Value>, runbook_api::session::Unavailable>,
    > {
        self.host.auth_internal(route, body)
    }
    fn webhook_host(&self) -> Self::WebhookHost {
        self.host.webhook_host()
    }
    fn images(&self) -> Self::Images {
        NoImages
    }
    fn try_image_slot(&self) -> Option<()> {
        Some(())
    }
    fn slack_app(&self) -> Option<runbook_core::slack::SlackApp> {
        self.host.slack_app()
    }
    async fn slack_api(
        &self,
        token: &str,
        method: &str,
        query: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        self.host.slack_api(token, method, query, body).await
    }
    async fn slack_respond(&self, url: &str, body: &serde_json::Value) -> Result<(), String> {
        self.host.slack_respond(url, body).await
    }
    fn wall_clock_ms(&self) -> i64 {
        self.host.now_ms()
    }
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), NoEntropy> {
        self.host.fill_random(bytes)
    }
    /// AUTH3 windows, shared by every isolate (gate fix B2).
    async fn rate_limit(
        &self,
        key: &str,
        window_ms: u64,
        limit: u32,
    ) -> Result<(), runbook_api::backends::Limited> {
        use crate::limits::RateLimits;
        self.host
            .limits()
            .hit(key, window_ms, limit, self.host.now_ms())
            .await
            .map_err(runbook_api::backends::Limited::Retry)
    }

    async fn execution_owner(
        &self,
        org: &OrganizationId,
        execution: &ExecutionId,
    ) -> Result<Option<TenantScope>, StoreError> {
        let found =
            runbook_store_d1::catalog::execution_owner(&self.for_org(org), execution).await?;
        Ok(found.filter(|scope| &scope.org_id == org))
    }
    /// CF2-9: the execution-token route row (hash → run and tenant). It
    /// routes only; the run's object checks the token itself.
    async fn execution_scope(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<TenantScope>, CredentialError> {
        Ok(self
            .credentials()
            .execution_route(hash)
            .await?
            .map(|(scope, _)| scope))
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
        self.credentials()
            .issue(
                &issue.record,
                &issue.metadata,
                &issue.principal,
                &issue.key,
                &issue.payload,
                &issue.response,
            )
            .await
            .map(issue_outcome)
    }
    async fn token_insert_issued(
        &self,
        record: &CredentialRecord,
        metadata: &TokenMetadata,
    ) -> Result<(), CredentialError> {
        self.credentials().insert_issued(record, metadata).await
    }
    async fn token_metadata(
        &self,
        org: &OrganizationId,
        id: &runbook_model::TokenId,
    ) -> Result<TokenMetadata, CredentialError> {
        self.credentials().metadata(org, id).await
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
    /// A step run whose claim lease expired: the lease is the run's
    /// object's, so outside it this explains nothing (`None`); inside it
    /// the object answers.
    async fn expired_step_run(
        &self,
        org: &OrganizationId,
        step_run: &StepRunId,
    ) -> Result<Option<(ExecutionId, ProjectId)>, StoreError> {
        let Some((execution, project)) = self
            .catalog(org_scope(org))
            .step_run_execution(org, step_run)
            .await?
        else {
            return Ok(None);
        };
        let scope = TenantScope {
            org_id: org.clone(),
            project_id: project.clone(),
        };
        let Ok(object) = self.host.objects().object(&execution, &scope) else {
            return Ok(None);
        };
        let now = object.now_ms();
        let run = match object.load(&execution).await {
            Ok(run) => run,
            Err(StoreError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(run
            .claims
            .get(step_run)
            .filter(|claim| claim.lease_deadline_at_ms.is_some_and(|at| at <= now))
            .map(|_| (execution, project)))
    }
    async fn memory_project(
        &self,
        org: &OrganizationId,
        id: &MemoryId,
    ) -> Result<Option<ProjectId>, StoreError> {
        runbook_store_d1::D1Memory::new(self.for_org(org))
            .memory_project(org, id)
            .await
    }
    async fn runbook_project(
        &self,
        org: &OrganizationId,
        id: &RunbookId,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.catalog(org_scope(org)).runbook_project(org, id).await
    }
    async fn me_tasks(
        &self,
        scope: &MeTasksScope,
        filters: &InboxFilters,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MeTasksPage, StoreError> {
        runbook_store_d1::me_tasks::me_tasks(
            &self.for_org(&scope.org_id),
            scope,
            filters,
            cursor,
            limit,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// B4.3 authoring and catalog stores.

#[derive(Clone)]
pub struct CfAuthoring<H: CfHost> {
    backends: CfBackends<H>,
}
pub struct CfAuthoringStore<H: CfHost> {
    store: CfExecution<H>,
    backends: CfBackends<H>,
}

impl<H: CfHost> AuthoringFactory for CfAuthoring<H> {
    type Store = CfAuthoringStore<H>;
    fn scoped(&self, scope: TenantScope) -> Self::Store {
        CfAuthoringStore {
            store: self.backends.execution(scope),
            backends: self.backends.clone(),
        }
    }
    async fn project_for_runbook(
        &self,
        org: &OrganizationId,
        id: &RunbookId,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.backends
            .catalog(org_scope(org))
            .runbook_project(org, id)
            .await
    }
}

fn err(_: runbook_store_d1::D1Error) -> StoreError {
    StoreError::Backend("d1.authoring")
}

impl<H: CfHost> AuthoringStore for CfAuthoringStore<H> {
    type Catalog = CfExecution<H>;
    fn catalog(&self) -> &Self::Catalog {
        &self.store
    }
    async fn runner_agents(&self) -> Result<runbook_core::authoring::RunnerAgents, StoreError> {
        use runbook_core::{agent_directory::AgentDirectory, runners::RunnerError};
        let scope = self.store.scope();
        let store = self.backends.runners();
        let error = |error| match error {
            RunnerError::Store(error) => error,
            _ => StoreError::Backend("d1.authoring"),
        };
        Ok(runbook_core::authoring::RunnerAgents {
            agents: store
                .project_agents(&scope.org_id, &scope.project_id)
                .await
                .map_err(error)?,
            default_agent: store
                .default_agent(&scope.org_id, &scope.project_id)
                .await
                .map_err(error)?,
        })
    }
    async fn catalog_runbooks(&self) -> Result<Vec<RunbookRecord>, StoreError> {
        let scope = self.store.scope();
        self.store
            .catalog()
            .db()
            .all(
                prepare(
                    "SELECT runbook_id, slug, origin, archived FROM runbook WHERE org_id = ? \
                     AND project_id = ? ORDER BY runbook_id DESC",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| {
                Ok(RunbookRecord {
                    runbook_id: row.parse(0).map_err(err)?,
                    slug: row.string(1).map_err(err)?,
                    origin: match row.text(2).map_err(err)? {
                        "git" => runbook_core::catalog::RunbookOrigin::Git,
                        _ => runbook_core::catalog::RunbookOrigin::Ui,
                    },
                    archived: row.flag(3).map_err(err)?,
                })
            })
            .collect()
    }
    /// UI-1c: one point read of the unique slug index.
    async fn runbook_by_slug(&self, slug: &str) -> Result<Option<RunbookRecord>, StoreError> {
        self.store.catalog().runbook_by_slug(slug).await
    }
    /// UI-1b: from rows (D1's stored listings), no source read.
    async fn runbook_listing_page(
        &self,
        query: &runbook_core::listing::ListingQuery,
    ) -> Result<runbook_core::listing::ListingPage, StoreError> {
        self.store.catalog().runbook_listing_page(query).await
    }
    async fn latest_published_digests(
        &self,
        runbook_ids: &[RunbookId],
    ) -> Result<std::collections::BTreeMap<RunbookId, (std::num::NonZeroU32, [u8; 32])>, StoreError>
    {
        self.store
            .catalog()
            .latest_published_digests(runbook_ids)
            .await
    }
    async fn version_data(
        &self,
        id: &RunbookId,
        selector: Option<VersionSelector>,
    ) -> Result<VersionRecord, StoreError> {
        let view = self.store.resolve_version(id, selector).await?;
        self.store
            .version_record(id, &view.runbook_version_id)
            .await?
            .ok_or(StoreError::NotFound)
    }
    async fn skill_versions(&self) -> Result<Vec<SkillVersion>, StoreError> {
        let scope = self.store.scope();
        self.store
            .catalog()
            .db()
            .all(
                prepare(
                    "SELECT name, description, hash, manifest, provenance FROM skill_version \
                     WHERE org_id = ? AND project_id = ? ORDER BY name, seq DESC",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| {
                Ok(SkillVersion {
                    name: row.string(0).map_err(err)?,
                    description: row.string(1).map_err(err)?,
                    hash: row.digest(2).map_err(err)?,
                    manifest: row.json(3).map_err(err)?,
                    provenance: row.json(4).map_err(err)?,
                })
            })
            .collect()
    }
    /// The directory revision is checked first and again when the replay
    /// row is written (a guard), so a concurrent directory change refuses
    /// the replay. D1 cannot hold the revision across the catalog write, so
    /// the write and its replay row are two batches: a retry after a lost
    /// replay write answers from the current state (recorded in CF3).
    async fn commit_authoring(
        &self,
        revision: u64,
        replay: Option<RequestReplay>,
        command: AuthoringCommand,
    ) -> Result<StoredResponse, StoreError> {
        let scope = self.store.scope().clone();
        // The directory revision and the request replays are the control
        // database's (CF11); the catalog write is the shard's.
        let db = self.store.control();
        let directory = self.backends.replay_directory(db.clone());
        use runbook_core::directory_operations::DirectoryOperations;
        if let Some(r) = &replay {
            match directory
                .lookup_replay(&scope.org_id, revision, r)
                .await
                .map_err(|_| StoreError::Backend("authoring.replay"))?
            {
                ReplayResult::Applied(response) => return Ok(response),
                ReplayResult::KeyReused => {
                    return Err(StoreError::InvalidBatch("request.idempotency_key_reused"));
                }
                ReplayResult::Missing => {}
                ReplayResult::Conflict => {
                    return Err(StoreError::InvalidBatch("directory.concurrent_change"));
                }
            }
        } else {
            let current = directory
                .snapshot(&scope.org_id)
                .await
                .map_err(|_| StoreError::NotFound)?
                .revision;
            if current != revision {
                return Err(StoreError::InvalidBatch("directory.concurrent_change"));
            }
        }
        let store = &self.store;
        let response = match command {
            AuthoringCommand::Create {
                record,
                source,
                diagnostics,
            } => draft_response(
                store.create_runbook(record, source).await?,
                diagnostics,
                201,
            ),
            AuthoringCommand::Save {
                runbook_id,
                revision,
                source,
                diagnostics,
            } => match store
                .save_draft(&runbook_id, revision, source.clone())
                .await?
            {
                runbook_core::CommitResult::Conflict => {
                    return Err(StoreError::InvalidBatch("catalog.draft_conflict"));
                }
                runbook_core::CommitResult::Committed(next) => draft_response(
                    DraftView {
                        runbook_id,
                        source,
                        revision: next
                            .try_into()
                            .map_err(|_| StoreError::Backend("d1.invalid_revision"))?,
                    },
                    diagnostics,
                    200,
                ),
            },
            AuthoringCommand::Publish {
                runbook_id,
                revision,
                prepared,
            } => StoredResponse {
                status: 201,
                body: serde_json::json!(
                    store
                        .publish_revision(&runbook_id, revision, *prepared)
                        .await?
                ),
                etag: None,
            },
            AuthoringCommand::Sync {
                ops,
                archive,
                branch,
                commit_sha,
            } => {
                // D1 has no multi-statement transaction across these calls:
                // the batch was compiled whole first, and each runbook's
                // publish is idempotent, so a retry converges.
                let mut published = Vec::with_capacity(ops.len());
                for op in &ops {
                    if let Some(record) = &op.create {
                        store
                            .create_runbook(record.clone(), op.prepared.source.clone())
                            .await?;
                    }
                    published.push(
                        store
                            .publish_git(
                                &op.runbook_id,
                                (*op.prepared).clone(),
                                branch.clone(),
                                commit_sha.clone(),
                            )
                            .await?,
                    );
                }
                for (_, id) in &archive {
                    store.archive_git(id).await?;
                }
                runbook_core::authoring::sync_response(
                    &ops, &published, &archive, branch, commit_sha,
                )
            }
            AuthoringCommand::Skill { version } => {
                let version = store.record_skill_version(version).await?;
                let latest = store
                    .latest_skills()
                    .await?
                    .iter()
                    .any(|s| s.name == version.name && s.hash == version.hash);
                StoredResponse {
                    status: 201,
                    body: serde_json::json!(skill_view(version, latest)),
                    etag: None,
                }
            }
        };
        if let Some(r) = replay {
            let now = db.now_ms();
            let expires_at = now.saturating_add(86_400_000);
            // CF12b: this organization's expired replay rows kept in R2 go
            // first, objects then rows (the batch below keeps them).
            let blobs = CfReplayBlobs(self.backends.clone());
            let _ = runbook_store_d1::replay::sweep(&db, Some(&scope.org_id), now, &blobs, 1).await;
            // CF12 review round 1: a response too large for its D1 row (a
            // runbook's 1 MiB source, escaped, or a long skill manifest) is
            // kept in R2 first, and the row names it. CF12b: under the
            // project's replay prefix, which no export reads, named for
            // this row so that its sweep takes only its own.
            // CF12b review round 2: the writer's nonce in the object's name.
            let mut nonce = [0u8; 16];
            self.backends
                .host()
                .fill_random(&mut nonce)
                .map_err(|_| StoreError::Backend("authoring.replay"))?;
            let reply = runbook_store_d1::replay::stored_reply(
                &response,
                &scope.project_id,
                runbook_store_d1::replay::ReplyRow {
                    principal: r.principal.as_str(),
                    operation: r.operation.as_str(),
                    key: r.key.as_str(),
                    expires_at,
                    nonce,
                },
            )
            .map_err(|_| StoreError::Backend("d1.encode"))?;
            let row = runbook_store_d1::replay::RowKey {
                org: &scope.org_id,
                principal: r.principal.as_str(),
                operation: r.operation.as_str(),
                key: r.key.as_str(),
            };
            // CF12b review round 1: an expired row with this key that the
            // sweep has not reached is replaced below (not kept by `ON
            // CONFLICT DO NOTHING`, which dropped this response); its
            // object goes first.
            let _ = runbook_store_d1::replay::forget_expired(&db, row, now, &blobs).await;
            if let Some(object) = &reply.blob {
                store
                    .blobs()
                    .reply_put(&object.name, object.bytes.clone())
                    .await
                    .map_err(|_| StoreError::Backend("authoring.replay"))?;
            }
            let written = db
                .batch(vec![
                    guard(
                        "conflict",
                        "EXISTS (SELECT 1 FROM directory_revision WHERE org_id = ? AND revision = ?)",
                    )
                    .bind(scope.org_id.as_str())
                    .bind(
                        i64::try_from(revision)
                            .map_err(|_| StoreError::Backend("d1.revision"))?,
                    ),
                    prepare(format!(
                        "DELETE FROM directory_request WHERE org_id = ? AND expires_at <= ? AND {}",
                        runbook_store_d1::replay::SWEEPABLE
                    ))
                    .bind(scope.org_id.as_str())
                    .bind(now),
                    runbook_store_d1::replay::record(
                        row,
                        &r.payload,
                        reply.column.clone(),
                        expires_at,
                        now,
                    ),
                ])
                .await;
            // CF12b review round 1: the object stored first goes again when
            // no row names it (a concurrent request with this key landed
            // first, or the batch failed, revision guard included).
            if let Some(object) = &reply.blob {
                runbook_store_d1::replay::release_unrecorded(
                    &db,
                    row,
                    &reply.column,
                    object,
                    &blobs,
                )
                .await;
            }
            written.map_err(|_| StoreError::Backend("authoring.replay"))?;
        }
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// B5 starts, reads, search and reports (ported from runbook-server's
// Postgres backend, generic over the composed store).

#[derive(Clone)]
pub struct CfExecutions<H: CfHost> {
    backends: CfBackends<H>,
}

fn start_error(e: StartError) -> Error {
    match e {
        StartError::Store(StoreError::InvalidBatch(crate::compose::REPLAYED_ELSEWHERE)) => {
            Error::new(409, crate::compose::REPLAYED_ELSEWHERE)
        }
        StartError::Store(e) => store_error(e),
        StartError::VersionNotFound => Error::new(404, "runbook.version_not_found"),
        StartError::ConcurrencyTooManyHolders => {
            Error::new(409, runbook_core::concurrency::TOO_MANY_HOLDERS_CODE)
        }
        StartError::ConcurrencyContended => {
            Error::new(409, runbook_core::concurrency::CONTENDED_CODE)
        }
        StartError::ConcurrencyHolderUnavailable(_) => {
            Error::new(409, runbook_core::concurrency::HOLDER_UNAVAILABLE_CODE)
        }
        StartError::Diagnostics(diagnostics) => Error {
            status: 422,
            code: "start.invalid",
            diagnostics,
        },
        StartError::InvalidData(errors) => Error {
            status: 422,
            code: StartError::INVALID_DATA_CODE,
            diagnostics: errors
                .into_iter()
                .map(|e| Diagnostic {
                    path: e.path,
                    code: e.code,
                    message: e.message,
                    line: None,
                    column: None,
                })
                .collect(),
        },
    }
}

impl<H: CfHost> CfExecutions<H> {
    fn store(&self, scope: &TenantScope) -> CfExecution<H> {
        self.backends.execution(scope.clone())
    }
    async fn undelivered(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<Vec<UndeliveredNotification>, Error> {
        runbook_core::webhooks::undelivered_for(&self.backends.webhooks(), scope, execution)
            .await
            .map_err(store_error)
    }
    async fn names(&self, scope: &TenantScope) -> Result<BTreeMap<UserId, String>, Error> {
        let snapshot = self
            .backends
            .directory()
            .snapshot(&scope.org_id)
            .await
            .map_err(|_| Error::new(503, "storage.unavailable"))?;
        Ok(snapshot
            .users
            .iter()
            .map(|(id, user)| (id.clone(), user.name.clone()))
            .collect())
    }
    async fn document(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<(runbook_engine::ExecutionState, ExecutionDocumentView), Error> {
        let store = self.store(scope);
        let run = store.load(execution).await.map_err(store_error)?;
        let record = store
            .start_record(execution)
            .await
            .map_err(store_error)?
            .ok_or_else(|| Error::new(404, "resource.not_found"))?;
        let admission = effective_admission(
            &store,
            execution,
            store.admission(execution).await.map_err(store_error)?,
        )
        .await?;
        let state = restore_state(&store, &record, &run).await?;
        let mut header = current_view(&record, &run, &state, admission)?;
        fill_identity(&store, scope, execution, &record, &mut header).await;
        header.notifications_not_delivered = self.undelivered(scope, execution).await?;
        let mut document =
            runbook_core::execution_document::build_document(&state, header, |step| {
                format!("/v1/step-runs/{step}/logs")
            });
        runbook_core::execution_document::mark_dropped_proposals(
            &store,
            scope,
            &mut document.sections,
        )
        .await
        .map_err(store_error)?;
        Ok((state, document))
    }
}

impl<H: CfHost> ExecutionBackend for CfExecutions<H> {
    async fn search(
        &self,
        scope: &TenantScope,
        query: runbook_api::search::SearchQuery,
        origin: &str,
    ) -> Result<serde_json::Value, Error> {
        crate::run_search::search(&self.backends, scope, query, origin).await
    }
    async fn execution_project(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<Option<ProjectId>, Error> {
        self.store(scope)
            .execution_project(&scope.org_id, execution)
            .await
            .map_err(store_error)
    }
    /// ARC2 is not on D1 yet: no run keeps a summary after deletion here.
    async fn summary_retained(
        &self,
        _scope: &TenantScope,
        _execution: &ExecutionId,
    ) -> Result<bool, Error> {
        Ok(false)
    }
    async fn tombstone_project(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<Option<ProjectId>, Error> {
        self.store(scope)
            .tombstone_project(&scope.org_id, execution)
            .await
            .map_err(store_error)
    }
    /// A saved start answers only once its run's object holds it; a
    /// reservation whose object write was lost (review fix 2) is finished
    /// by `start` under the saved ID, not answered as a 503.
    async fn find_replay(
        &self,
        scope: &TenantScope,
        identity: &StartIdentity,
    ) -> Result<Option<ExecutionView>, Error> {
        let store = self.store(scope);
        let Some(replay) = store
            .find_replay(&identity.key)
            .await
            .map_err(store_error)?
        else {
            return Ok(None);
        };
        if replay.principal != identity.principal || replay.request_hash != identity.request_hash {
            return Err(Error::new(409, "request.idempotency_key_reused"));
        }
        if store
            .start_record(&replay.outcome.execution_id)
            .await
            .map_err(store_error)?
            .is_none()
        {
            return Ok(None);
        }
        Ok(Some(initial_outcome(&store, scope, replay.outcome).await?))
    }
    async fn published_triggers(
        &self,
        scope: &TenantScope,
        runbook: &str,
    ) -> Result<
        Option<(
            runbook_core::triggers::PublishedVersion,
            std::rc::Rc<runbook_spec::ir::Triggers>,
        )>,
        Error,
    > {
        let rb = if let Ok(id) = runbook.parse::<RunbookId>() {
            id
        } else {
            match self
                .backends
                .authoring()
                .scoped(scope.clone())
                .catalog_runbooks()
                .await
                .map_err(store_error)?
                .into_iter()
                .find(|r| r.slug == runbook)
            {
                Some(found) => found.runbook_id,
                None => return Ok(None),
            }
        };
        runbook_core::triggers::published_triggers(&self.store(scope), scope, &rb)
            .await
            .map_err(store_error)
    }
    async fn start(
        &self,
        scope: &TenantScope,
        invocation: StartInvocation,
    ) -> Result<ExecutionView, Error> {
        let authoring = self.backends.authoring().scoped(scope.clone());
        let rb = if let Ok(id) = invocation.request.runbook.parse::<RunbookId>() {
            id
        } else {
            authoring
                .catalog_runbooks()
                .await
                .map_err(store_error)?
                .into_iter()
                .find(|r| r.slug == invocation.request.runbook)
                .map(|r| r.runbook_id)
                .ok_or_else(|| Error::new(404, "resource.not_found"))?
        };
        let store = self.store(scope);
        // A saved start under this key (same principal and body, checked by
        // `find_replay`) keeps its run ID and pinned version (review fixes
        // 1 and 2): this object finishes it if it is that run's, otherwise
        // the host re-sends the request to the saved run.
        let saved = store
            .find_replay(&invocation.identity.key)
            .await
            .map_err(store_error)?
            .filter(|r| {
                r.principal == invocation.identity.principal
                    && r.request_hash == invocation.identity.request_hash
            });
        let finishing = saved.is_some();
        let (execution_id, version) = match saved {
            Some(saved)
                if saved.outcome.execution_id != invocation.execution_id
                    && store
                        .objects()
                        .object(&saved.outcome.execution_id, scope)
                        .is_err() =>
            {
                return Err(Error::new(409, crate::compose::REPLAYED_ELSEWHERE));
            }
            Some(saved) => (
                saved.outcome.execution_id,
                Some(VersionSelector::Id(saved.outcome.runbook_version_id)),
            ),
            None => (invocation.execution_id, invocation.request.version),
        };
        let command = StartCommand {
            execution_id,
            runbook_id: rb,
            version,
            inputs: invocation.request.inputs,
            fields: invocation.request.fields,
            title: invocation.request.title,
            tags: invocation.request.tags,
            project_vars: invocation.project_vars,
            url: invocation.url,
            created_by: invocation.identity.principal.clone(),
            limits: runbook_engine::ExecutionLimits {
                max_step_runs: self.backends.host().step_cap(),
                ..Default::default()
            },
            idempotency_key: invocation.identity.key,
            principal: invocation.identity.principal,
            request_hash: invocation.identity.request_hash,
        };
        let commit = prepare_start_commit(&store, &store, &command)
            .await
            .map_err(start_error)?;
        let directory = self.backends.directory();
        let outcome = if finishing {
            // Core answers a saved replay without committing; here the saved
            // run's object is still empty, so its start is committed (D1
            // replays its half, the object initializes).
            store
                .commit_start(commit)
                .await
                .map_err(|e| start_error(e.into()))?
        } else {
            commit_start_with_policy(&store, &directory, scope, commit)
                .await
                .map_err(start_error)?
        };
        initial_outcome(&store, scope, outcome).await
    }
    async fn read(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<ExecutionView, Error> {
        let store = self.store(scope);
        let run = store.load(execution).await.map_err(store_error)?;
        let record = store
            .start_record(execution)
            .await
            .map_err(store_error)?
            .ok_or_else(|| Error::new(404, "resource.not_found"))?;
        let admission = effective_admission(
            &store,
            execution,
            store.admission(execution).await.map_err(store_error)?,
        )
        .await?;
        let state = restore_state(&store, &record, &run).await?;
        let mut view = current_view(&record, &run, &state, admission)?;
        fill_identity(&store, scope, execution, &record, &mut view).await;
        view.notifications_not_delivered = self.undelivered(scope, execution).await?;
        Ok(view)
    }
    /// B9 reruns (CF5). The original's start facts are read from its
    /// object (this one, when the request reached the original's object;
    /// the object protocol otherwise) and the new run is started like any
    /// start: its D1 half is the commit point and its own object is
    /// initialized through the protocol.
    async fn rerun(
        &self,
        scope: &TenantScope,
        invocation: RerunInvocation,
    ) -> Result<ExecutionView, Error> {
        let store = self.store(scope);
        let original = match store.start_record(&invocation.original).await {
            Ok(Some(original)) => original,
            Ok(None) | Err(StoreError::NotFound) => {
                return Err(Error::new(404, "resource.not_found"));
            }
            Err(error) => return Err(store_error(error)),
        };
        // Omitted version reruns the original's exact pinned version,
        // including a preview (which keeps a null version number).
        let selector = invocation
            .version
            .unwrap_or_else(|| VersionSelector::Id(original.version_id.clone()));
        let resolved = store
            .resolve_version(&original.runbook_id, Some(selector))
            .await
            .map_err(|e| match e {
                StoreError::NotFound => Error::new(404, "runbook.version_not_found"),
                other => store_error(other),
            })?;
        let target = store
            .version_record(&original.runbook_id, &resolved.runbook_version_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| Error::new(404, "runbook.version_not_found"))?;
        let bytes = store
            .get(&BlobKey::Shared {
                hash: target.snapshot_digest,
            })
            .await
            .map_err(store_error)?;
        let compiled = runbook_spec::restore(&bytes)
            .map_err(|_| Error::new(503, "execution.snapshot_invalid"))?;
        let initial = runbook_engine::InitialData {
            context: original.context.clone(),
            vars: Data::new(),
            reads: vec![],
        };
        let context = runbook_core::rerun::rerun_context(
            &initial,
            &compiled,
            runbook_core::rerun::RerunInvocation {
                url: invocation.url.clone(),
                created_by: invocation.identity.principal.clone(),
                project_vars: invocation.project_vars,
            },
        )
        .map_err(|rejected| Error {
            status: runbook_core::rerun::RerunRejected::STATUS,
            code: runbook_core::rerun::RerunRejected::CODE,
            diagnostics: rejected
                .errors
                .into_iter()
                .map(|e| Diagnostic {
                    path: e.path,
                    code: e.code,
                    message: e.message,
                    line: None,
                    column: None,
                })
                .collect(),
        })?;
        let command = StartCommand {
            execution_id: invocation.execution_id,
            runbook_id: original.runbook_id.clone(),
            version: Some(VersionSelector::Id(resolved.runbook_version_id)),
            inputs: context.inputs,
            fields: context.fields,
            project_vars: context.project_vars,
            title: original.context.title.clone(),
            tags: original.request_tags.clone(),
            url: invocation.url,
            created_by: invocation.identity.principal.clone(),
            // A rerun runs under today's cap, not the original's.
            limits: runbook_engine::ExecutionLimits {
                max_step_runs: self.backends.host().step_cap(),
                ..context.limits
            },
            idempotency_key: invocation.identity.key,
            principal: invocation.identity.principal,
            request_hash: invocation.identity.request_hash,
        };
        let mut commit = prepare_start_commit(&store, &store, &command)
            .await
            .map_err(start_error)?;
        commit.record.rerun_of = Some(invocation.original);
        let directory = self.backends.directory();
        let outcome = commit_start_with_policy(&store, &directory, scope, commit)
            .await
            .map_err(start_error)?;
        initial_outcome(&store, scope, outcome).await
    }
    async fn report(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<ExecutionDocumentView, Error> {
        Ok(self.document(scope, execution).await?.1)
    }
    async fn report_markdown(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<String, Error> {
        let (state, document) = self.document(scope, execution).await?;
        let names = self.names(scope).await?;
        Ok(runbook_core::execution_document::render_markdown_named(
            &state, &document, &names,
        ))
    }
    async fn report_junit(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<String, Error> {
        let (state, document) = self.document(scope, execution).await?;
        let names = self.names(scope).await?;
        Ok(runbook_core::report_junit::render_junit_named(
            &state, &document, &names,
        ))
    }
}

async fn fill_identity<S: CatalogStore>(
    store: &S,
    scope: &TenantScope,
    execution: &ExecutionId,
    record: &StartRecord,
    view: &mut ExecutionView,
) {
    view.project_id = Some(scope.project_id.clone());
    if let Ok(Some(runbook)) = store.read_runbook(&record.runbook_id).await {
        view.runbook_slug = Some(runbook.slug);
    }
    if let Ok(Some(summary)) = store.get_run(execution).await {
        view.created_at_ms = Some(summary.created_at_ms);
        view.created_by = Some(summary.created_by);
    }
}

async fn initial_outcome<S: StartStore + CatalogStore>(
    store: &S,
    scope: &TenantScope,
    outcome: StartOutcome,
) -> Result<ExecutionView, Error> {
    let record = store
        .start_record(&outcome.execution_id)
        .await
        .map_err(store_error)?
        .ok_or_else(|| Error::new(503, "start.record_missing"))?;
    let mut view = initial_view(
        &outcome.execution_id,
        &record,
        effective_admission(store, &outcome.execution_id, outcome.admission).await?,
    );
    if let Ok(Some(runbook)) = store.read_runbook(&record.runbook_id).await {
        view.runbook_slug = Some(runbook.slug);
    }
    view.project_id = Some(scope.project_id.clone());
    if let Ok(Some(summary)) = store.get_run(&outcome.execution_id).await {
        view.created_at_ms = Some(summary.created_at_ms);
    }
    Ok(view)
}

/// Unpooled runs are admitted at their start's activation time: the
/// `ExecutionStarted` event, or (before it) the run's creation time.
async fn effective_admission<S: ExecutionStore + CatalogStore>(
    store: &S,
    id: &ExecutionId,
    admission: Option<Admission>,
) -> Result<Option<Admission>, Error> {
    if admission.is_some() {
        return Ok(admission);
    }
    let summary = store
        .get_run(id)
        .await
        .map_err(store_error)?
        .map(|s| s.created_at_ms);
    let run = store.load(id).await.map_err(store_error)?;
    let started = run
        .events
        .iter()
        .find(|e| matches!(e.event, runbook_engine::Event::ExecutionStarted { .. }))
        .map(|e| e.at_ms);
    let at = started
        .or(summary)
        .or(run.events.first().map(|e| e.at_ms))
        .ok_or_else(|| Error::new(404, "resource.not_found"))?;
    Ok(Some(Admission {
        execution_id: id.clone(),
        state: AdmissionState::Admitted,
        admitted_at_ms: Some(at),
    }))
}

fn initial_view(
    id: &ExecutionId,
    record: &StartRecord,
    admission: Option<Admission>,
) -> ExecutionView {
    ExecutionView {
        overdue_steps: vec![],
        schema_revision: NonZeroU32::MIN,
        id: id.clone(),
        url: record.context.url.clone(),
        web_url: execution_web_url(&record.context.url, id),
        runbook_id: Some(record.runbook_id.clone()),
        runbook_slug: None,
        project_id: None,
        created_at_ms: None,
        created_by: Some(record.context.created_by.clone()),
        started_at_ms: None,
        completed_at_ms: None,
        runbook_version_id: record.version_id.clone(),
        runbook_version: record.version.map(NonZeroU32::get),
        title: record.context.title.clone().unwrap_or_default(),
        status: ExecutionStatus::Queued,
        phase: ExecutionPhase::Ordinary,
        admission: admission.map(admission_view),
        execution_deadline_at_ms: None,
        result: None,
        pending_terminal_status: None,
        terminal_reason: None,
        inputs: record.context.inputs.clone(),
        fields: record.context.fields.clone(),
        tags: record.context.tags.clone(),
        steps: vec![],
        amended: false,
        amendments: vec![],
        stale_step_ids: vec![],
        rerun_of: record.rerun_of.clone(),
        resume_brief: None,
        notifications_not_delivered: vec![],
        superseded_by: None,
        // LANG4: a child names its parent from its start, before it runs.
        parent: record
            .context
            .parent
            .as_ref()
            .map(runbook_core::execution_document::parent_view),
        progress: None,
        cost: None,
    }
}

fn admission_view(admission: Admission) -> ExecutionAdmissionView {
    ExecutionAdmissionView {
        state: match admission.state {
            AdmissionState::Queued => ExecutionAdmissionState::Queued,
            AdmissionState::Admitted => ExecutionAdmissionState::Admitted,
            AdmissionState::Released => ExecutionAdmissionState::Released,
        },
        admitted_at_ms: admission.admitted_at_ms,
    }
}

fn current_view(
    record: &StartRecord,
    run: &StoredExecution,
    state: &runbook_engine::ExecutionState,
    admission: Option<Admission>,
) -> Result<ExecutionView, Error> {
    let mut view = initial_view(&state.id, record, admission);
    if state.sequence == 0 {
        return Ok(view);
    }
    view.status = runbook_engine::reported_status(state);
    view.phase = if matches!(
        state.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    ) {
        ExecutionPhase::Completed
    } else {
        match state.phase {
            runbook_engine::ExecutionPhase::Ordinary => ExecutionPhase::Ordinary,
            runbook_engine::ExecutionPhase::Cleanup => ExecutionPhase::Cleanup,
        }
    };
    view.title = state.title.clone();
    view.inputs = state.inputs.clone();
    view.fields = state.fields.clone();
    view.tags = state.tags.clone();
    view.result = (!state.result.is_null()).then(|| state.result.clone());
    view.started_at_ms = state.started_at_ms;
    view.completed_at_ms = run.projection.completed_at_ms;
    view.execution_deadline_at_ms = state.execution_deadline_at_ms;
    view.pending_terminal_status = state.pending_terminal_cause.map(|c| match c {
        runbook_engine::TerminalCause::Succeeded => ExecutionStatus::Succeeded,
        runbook_engine::TerminalCause::Failed => ExecutionStatus::Failed,
        runbook_engine::TerminalCause::Cancelled => ExecutionStatus::Cancelled,
    });
    view.terminal_reason = matches!(
        view.status,
        ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
    .then(|| state.terminal_reason.clone())
    .flatten();
    for step in runbook_core::execution_document::runbook_order(state) {
        let Some(id) = &step.id else {
            continue;
        };
        let claim = run
            .claims
            .get(id)
            .filter(|_| step.status == StepStatus::Claimed)
            .map(|c| {
                NonZeroU64::new(c.generation)
                    .map(|generation| ClaimRef {
                        id: c.claim_id.clone(),
                        generation,
                    })
                    .ok_or_else(|| Error::new(503, "execution.claim_invalid"))
            })
            .transpose()?;
        view.steps.push(StepView {
            schema_revision: NonZeroU32::MIN,
            id: id.clone(),
            kind: state.node(&step.step_id).map(|node| {
                match &node.kind {
                    runbook_spec::ir::NodeKind::Task(_) => "task",
                    runbook_spec::ir::NodeKind::Approval(_) => "approval",
                    runbook_spec::ir::NodeKind::Group(_) => "group",
                    runbook_spec::ir::NodeKind::Parallel { .. } => "parallel",
                    runbook_spec::ir::NodeKind::Foreach(_) => "foreach",
                    runbook_spec::ir::NodeKind::Loop(_) => "loop",
                    runbook_spec::ir::NodeKind::Switch(_) => "switch",
                    runbook_spec::ir::NodeKind::Wait(_) => "wait",
                    runbook_spec::ir::NodeKind::Set(_) => "set",
                    runbook_spec::ir::NodeKind::End(_) => "end",
                    runbook_spec::ir::NodeKind::Call(_) => "call",
                }
                .to_owned()
            }),
            completed_at_ms: step.finished_at_ms,
            path: runbook_core::execution_document::path_of(step),
            status: step.status,
            verdict: step.verdict,
            outputs: step.outputs.clone(),
            summary: step.summary.clone(),
            actor: step.actor.clone(),
            claim,
            attempt: NonZeroU32::new(step.attempt)
                .ok_or_else(|| Error::new(503, "execution.attempt_invalid"))?,
            satisfied: step.satisfied,
            exhausted: step.exhausted,
            checklist: step.checklist.clone(),
            amended: false,
            amendments: vec![],
            stale: false,
            eligible_recipients: step
                .handoff
                .as_ref()
                .map(|h| h.eligible_recipients.clone())
                .unwrap_or_default(),
            // Set below from state, with the other state facts (LANG4).
            call: None,
            wait: None,
            actor_kind: None,
            assignees: vec![],
            branch: None,
            iterations: None,
            skip_reason: None,
            followup: None,
            runner: None,
            cost: None,
        });
    }
    runbook_core::execution_document::apply_amendment_facts(state, &mut view);
    Ok(view)
}

/// The blob a run's version snapshot is stored under.
pub fn snapshot_key(digest: [u8; 32]) -> BlobKey {
    BlobKey::Shared { hash: digest }
}

#[allow(dead_code)]
fn _assert_blob_store<S: BlobStore>() {}

/// UPD1: the schema heads this code migrates to, which `/v1/meta` reports:
/// the last D1 migration and each Durable Object class's last migration.
/// (Each store migrates on first use, so these are what the code brings a
/// database or object to, not a read of one.)
pub fn schema_heads() -> std::collections::BTreeMap<String, u64> {
    fn head<T>(list: &[T], version: impl Fn(&T) -> i64) -> u64 {
        list.last().map_or(0, |m| version(m).max(0) as u64)
    }
    [
        (
            "d1".to_owned(),
            head(runbook_store_d1::migrate::MIGRATIONS, |m| m.version),
        ),
        (
            "execution_do".to_owned(),
            head(runbook_store_do::migrate::MIGRATIONS, |m| m.version),
        ),
        (
            "org_do".to_owned(),
            head(runbook_store_do::org::ORG_MIGRATIONS, |m| m.version),
        ),
        (
            "webhook_do".to_owned(),
            head(runbook_store_do::webhook::WEBHOOK_MIGRATIONS, |m| m.version),
        ),
    ]
    .into()
}

/// The router's configuration on this edition: uploads are off until their
/// metadata is on D1 (CF5), so `/v1/meta` advertises none; sign-in is CF7's.
pub fn router_config(origin: &str, max_request_bytes: u64) -> runbook_api::router::RouterConfig {
    let limits = runbook_core::uploads::limits::cloudflare();
    runbook_api::router::RouterConfig {
        discovery: runbook_api::discovery::DiscoveryConfig {
            origin: origin.to_owned(),
            minimum_cli_version: "0.1.0".into(),
            max_request_bytes,
            version: runbook_api::VERSION.into(),
            schema: schema_heads(),
            theme: None,
            trace_sample_ratio: None,
            step_runs: None,
        },
        authoring: runbook_api::authoring::AuthoringLimits::default(),
        uploads: runbook_api::evidence::UploadSettings {
            enabled: false,
            signing_key: None,
            usercontent_origin: None,
            limits: limits.clone(),
            sniff: None,
        },
        upload_limits: limits,
        sign_in: None,
    }
}
