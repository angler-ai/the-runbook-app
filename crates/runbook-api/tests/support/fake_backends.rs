//! A `Backends` over `runbook-core`'s in-memory stores, for router tests.
//! Host-specific backends (executions, authoring, archives) answer every
//! call with a distinctive `fake.*` error, so a response shows which one
//! was reached.
#![allow(dead_code)]
use runbook_api::{
    archives::ArchiveBackend,
    backends::{Backends, IssueOutcome, NoEntropy, TokenIssue},
    discovery::DiscoveryConfig,
    evidence::UploadSettings,
    executions::{
        ExecutionApiError, ExecutionBackend, RerunInvocation, StartIdentity, StartInvocation,
    },
    router::{Router, RouterConfig, SignIn},
    webhooks::WebhookHost,
};
use runbook_core::{
    StoreError,
    archives::job::RetentionPolicy,
    authoring::{AuthoringCommand, AuthoringFactory, AuthoringStore},
    catalog::{RunbookRecord, SkillVersion, TenantScope, VersionRecord},
    credentials::{
        CredentialError, CredentialRecord, SecretHash, issuance::TokenMetadata,
        memory::MemoryCredentials,
    },
    device_codes::DeviceCodeRecord,
    directory::{DirectoryError, memory::MemoryDirectory},
    directory_operations::{RequestReplay, StoredResponse},
    inbox::{InboxFilters, MeTasksPage, MeTasksScope},
    store::{ExecutionProjection, memory::MemoryExecutionStore},
    webhooks::{SealedSecret, SecretSealer, TargetPolicy, memory::MemoryWebhooks},
};
use runbook_model::*;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const ORIGIN: &str = "https://runbooks.example";

fn fake(area: &'static str) -> ExecutionApiError {
    ExecutionApiError::new(599, area)
}

#[derive(Clone)]
pub struct Fake {
    pub store: MemoryExecutionStore,
    pub directory: MemoryDirectory,
    pub credentials: MemoryCredentials,
    issued_tokens: Rc<RefCell<Vec<(CredentialRecord, TokenMetadata)>>>,
    pub webhooks: MemoryWebhooks,
    pub notify: runbook_core::notify::memory::MemoryNotify,
    pub random: Rc<Cell<u8>>,
    /// MCP2: the auth service's connected-app routes: each call
    /// (route, body), and what the next one answers.
    pub apps: Rc<std::cell::RefCell<FakeApps>>,
    /// SLACK1 stage 2: the app, `users.info`'s answer, and replies sent.
    pub slack: Rc<std::cell::RefCell<FakeSlack>>,
    /// What `published_versions` answers (schedule passes).
    pub published: Rc<std::cell::RefCell<Vec<runbook_core::triggers::PublishedVersion>>>,
}

#[derive(Default)]
pub struct FakeSlack {
    pub app: Option<runbook_core::slack::SlackApp>,
    pub users_info: Option<Value>,
    /// `auth.test`'s `team_id`: the bot token's workspace (`T1` when unset).
    pub team_id: Option<String>,
    pub replies: Vec<(String, Value)>,
}

/// The connected-app calls made, and the next answer.
pub type FakeApps = (Vec<(String, Value)>, Option<Value>);

impl Fake {
    pub fn new() -> Self {
        let scope = TenantScope {
            org_id: format!("org_{:026}", 1).parse().unwrap(),
            project_id: format!("prj_{:026}", 1).parse().unwrap(),
        };
        let projection = ExecutionProjection {
            execution_id: format!("exe_{:026}", 1).parse().unwrap(),
            runbook_id: format!("rb_{:026}", 1).parse().unwrap(),
            version_id: format!("rbv_{:026}", 1).parse().unwrap(),
            compiled_hash: [0; 32],
            engine_version: runbook_engine::ENGINE_VERSION,
            sequence: 0,
            status: ExecutionStatus::Queued,
            phase: runbook_engine::ExecutionPhase::Ordinary,
            completed_at_ms: None,
            result: Value::Null,
            outputs: Default::default(),
            steps: vec![],
        };
        Self {
            store: MemoryExecutionStore::new(scope, projection, 1_000),
            directory: MemoryDirectory::default(),
            credentials: MemoryCredentials::default(),
            issued_tokens: Rc::default(),
            webhooks: MemoryWebhooks::new(1_000),
            notify: runbook_core::notify::memory::MemoryNotify::new(1_000_000),
            random: Rc::default(),
            apps: Rc::default(),
            slack: Rc::default(),
            published: Rc::default(),
        }
    }
}

impl Fake {
    /// Org 1 with an owner who is admin of project 1, and that owner's
    /// personal token with every scope. Returns the token.
    pub async fn seed(&self) -> String {
        use runbook_core::credentials::{
            ActorKind, CredentialPrincipal, CredentialStore, Scope, TokenKind, encode_secret,
            secret_hash,
        };
        use runbook_core::directory::{
            DirectoryCommit, DirectoryMutation, DirectoryStore, OrgRole, ProjectRecord, UserRecord,
        };
        let id = |prefix: &str| format!("{prefix}_{:026}", 1);
        self.directory
            .commit(DirectoryCommit {
                org_id: id("org").parse().unwrap(),
                expected_revision: 0,
                mutations: vec![
                    DirectoryMutation::CreateOrganization {
                        title: "Org".into(),
                    },
                    DirectoryMutation::CreateUser(UserRecord {
                        id: id("usr").parse().unwrap(),
                        name: "Owner".into(),
                        active: true,
                        role: OrgRole::Owner,
                    }),
                    DirectoryMutation::CreateProject(ProjectRecord {
                        id: id("prj").parse().unwrap(),
                        org_id: id("org").parse().unwrap(),
                        slug: "api".into(),
                        title: "API".into(),
                    }),
                    DirectoryMutation::GrantProjectRole {
                        project_id: id("prj").parse().unwrap(),
                        user_id: id("usr").parse().unwrap(),
                        role: ProjectRole::Admin,
                    },
                ],
            })
            .await
            .unwrap();
        let secret = encode_secret(TokenKind::Personal, [1; 32]);
        self.credentials
            .insert(CredentialRecord {
                id: id("tok").parse().unwrap(),
                kind: TokenKind::Personal,
                hash: secret_hash(&secret),
                principal: CredentialPrincipal::User {
                    user_id: id("usr").parse().unwrap(),
                    actor: ActorKind::Human,
                },
                org_id: id("org").parse().unwrap(),
                scopes: Scope::ALL.into(),
                projects: None,
                expires_at: Some(i64::MAX),
                revoked_at: None,
                last_used_at: None,
            })
            .await
            .unwrap();
        secret
    }
}

pub fn router(sign_in: bool) -> Router<Fake> {
    Router::new(
        Fake::new(),
        RouterConfig {
            discovery: DiscoveryConfig {
                origin: ORIGIN.into(),
                minimum_cli_version: "0.1.0".into(),
                max_request_bytes: 1 << 20,
                version: runbook_api::VERSION.into(),
                schema: Default::default(),
                theme: None,
                step_runs: None,
            },
            authoring: Default::default(),
            uploads: UploadSettings {
                enabled: false,
                signing_key: None,
                usercontent_origin: None,
                limits: runbook_api::discovery::self_hosted_upload_limits(),
                sniff: None,
            },
            upload_limits: runbook_api::discovery::self_hosted_upload_limits(),
            sign_in: sign_in.then(SignIn::default),
        },
    )
}

#[derive(Clone)]
pub struct FakeExecutions;
impl ExecutionBackend for FakeExecutions {
    async fn search(
        &self,
        _: &TenantScope,
        _: runbook_api::search::SearchQuery,
        _origin: &str,
    ) -> Result<Value, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
    async fn execution_project(
        &self,
        _: &TenantScope,
        _: &ExecutionId,
    ) -> Result<Option<ProjectId>, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
    async fn summary_retained(
        &self,
        _: &TenantScope,
        _: &ExecutionId,
    ) -> Result<bool, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
    async fn tombstone_project(
        &self,
        _: &TenantScope,
        _: &ExecutionId,
    ) -> Result<Option<ProjectId>, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
    async fn find_replay(
        &self,
        _: &TenantScope,
        _: &StartIdentity,
    ) -> Result<Option<ExecutionView>, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
    async fn start(
        &self,
        _: &TenantScope,
        _: StartInvocation,
    ) -> Result<ExecutionView, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
    async fn read(
        &self,
        _: &TenantScope,
        _: &ExecutionId,
    ) -> Result<ExecutionView, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
    async fn published_triggers(
        &self,
        _: &TenantScope,
        _: &str,
    ) -> Result<
        Option<(
            runbook_core::triggers::PublishedVersion,
            std::rc::Rc<runbook_spec::ir::Triggers>,
        )>,
        ExecutionApiError,
    > {
        Err(fake("fake.executions"))
    }
    async fn rerun(
        &self,
        _: &TenantScope,
        _: RerunInvocation,
    ) -> Result<ExecutionView, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
    async fn report(
        &self,
        _: &TenantScope,
        _: &ExecutionId,
    ) -> Result<ExecutionDocumentView, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
    async fn report_markdown(
        &self,
        _: &TenantScope,
        _: &ExecutionId,
    ) -> Result<String, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
    async fn report_junit(
        &self,
        _: &TenantScope,
        _: &ExecutionId,
    ) -> Result<String, ExecutionApiError> {
        Err(fake("fake.executions"))
    }
}

#[derive(Clone)]
pub struct FakeAuthoring(MemoryExecutionStore);
impl AuthoringStore for FakeAuthoring {
    type Catalog = MemoryExecutionStore;
    fn catalog(&self) -> &MemoryExecutionStore {
        &self.0
    }
    async fn catalog_runbooks(&self) -> Result<Vec<RunbookRecord>, StoreError> {
        Err(StoreError::Backend("fake.authoring"))
    }
    async fn version_data(
        &self,
        _: &RunbookId,
        _: Option<VersionSelector>,
    ) -> Result<VersionRecord, StoreError> {
        Err(StoreError::Backend("fake.authoring"))
    }
    async fn skill_versions(&self) -> Result<Vec<SkillVersion>, StoreError> {
        Err(StoreError::Backend("fake.authoring"))
    }
    async fn commit_authoring(
        &self,
        _: u64,
        _: Option<RequestReplay>,
        _: AuthoringCommand,
    ) -> Result<StoredResponse, StoreError> {
        Err(StoreError::Backend("fake.authoring"))
    }
}
impl AuthoringFactory for FakeAuthoring {
    type Store = FakeAuthoring;
    fn scoped(&self, _: TenantScope) -> FakeAuthoring {
        self.clone()
    }
    async fn project_for_runbook(
        &self,
        _: &OrganizationId,
        _: &RunbookId,
    ) -> Result<Option<ProjectId>, StoreError> {
        Err(StoreError::Backend("fake.authoring"))
    }
}

#[derive(Clone)]
pub struct FakeArchives(MemoryExecutionStore);
impl ArchiveBackend for FakeArchives {
    type Store = MemoryExecutionStore;
    async fn runbook_project(
        &self,
        _: &OrganizationId,
        _: &RunbookId,
    ) -> Result<Option<ProjectId>, StoreError> {
        Err(StoreError::Backend("fake.archives"))
    }
    async fn archived_run_project(
        &self,
        _: &OrganizationId,
        _: &ExecutionId,
    ) -> Result<Option<ProjectId>, StoreError> {
        Err(StoreError::Backend("fake.archives"))
    }
    fn store(&self, _: &TenantScope) -> MemoryExecutionStore {
        self.0.clone()
    }
    fn policy(&self, _: &ProjectId) -> RetentionPolicy {
        RetentionPolicy::default()
    }
}

pub struct FakeSealer;
impl SecretSealer for FakeSealer {
    fn seal(&self, _: &str, _: &str) -> Result<SealedSecret, StoreError> {
        Err(StoreError::Backend("fake.sealer"))
    }
    fn open(&self, _: &str, _: &SealedSecret) -> Result<String, StoreError> {
        Err(StoreError::Backend("fake.sealer"))
    }
}
pub struct FakeHost;
impl WebhookHost for FakeHost {
    type Sealer = FakeSealer;
    fn sealer(&self) -> Option<&FakeSealer> {
        None
    }
    fn random32(&self) -> Option<[u8; 32]> {
        None
    }
    fn target_policy(&self) -> TargetPolicy {
        TargetPolicy::default()
    }
    async fn vet_target(&self, _: &str) -> Result<(), &'static str> {
        Err("fake.vet")
    }
}

impl Backends for Fake {
    type Directory = MemoryDirectory;
    type Credentials = MemoryCredentials;
    type Execution = MemoryExecutionStore;
    type Webhooks = MemoryWebhooks;
    type Images = ();
    type ImagePermit = ();
    type WebhookHost = FakeHost;
    type Executions = FakeExecutions;
    type Authoring = FakeAuthoring;
    type Archives = FakeArchives;
    type Runners = runbook_core::runners::Unsupported;
    type Notify = runbook_core::notify::memory::MemoryNotify;

    fn directory(&self) -> MemoryDirectory {
        self.directory.clone()
    }
    fn credentials(&self) -> MemoryCredentials {
        self.credentials.clone()
    }
    fn execution(&self, scope: TenantScope) -> MemoryExecutionStore {
        self.store.scoped(scope)
    }
    fn webhooks(&self) -> MemoryWebhooks {
        self.webhooks.clone()
    }
    fn notify(&self) -> Self::Notify {
        self.notify.clone()
    }
    fn executions(&self) -> FakeExecutions {
        FakeExecutions
    }
    fn authoring(&self) -> FakeAuthoring {
        FakeAuthoring(self.store.clone())
    }
    fn runners(&self) -> Self::Runners {
        runbook_core::runners::Unsupported
    }
    fn archives(&self) -> FakeArchives {
        FakeArchives(self.store.clone())
    }
    async fn published_versions(
        &self,
    ) -> Result<Vec<runbook_core::triggers::PublishedVersion>, runbook_core::StoreError> {
        Ok(self.published.borrow().clone())
    }
    fn auth_methods(&self) -> Vec<String> {
        vec!["email".into()]
    }
    async fn auth_internal(
        &self,
        route: &'static str,
        body: Value,
    ) -> Result<Option<Value>, runbook_api::session::Unavailable> {
        let mut apps = self.apps.borrow_mut();
        apps.0.push((route.to_owned(), body));
        Ok(apps.1.clone())
    }
    fn webhook_host(&self) -> FakeHost {
        FakeHost
    }
    fn images(&self) {}
    fn try_image_slot(&self) -> Option<()> {
        Some(())
    }
    fn wall_clock_ms(&self) -> i64 {
        1_000
    }
    fn slack_app(&self) -> Option<runbook_core::slack::SlackApp> {
        self.slack.borrow().app.clone()
    }
    async fn slack_api(
        &self,
        _: &str,
        method: &str,
        _: &[(&str, &str)],
        _: Option<&Value>,
    ) -> Result<Value, String> {
        let slack = self.slack.borrow();
        match (method, slack.users_info.clone()) {
            ("auth.test", _) => Ok(
                serde_json::json!({"ok": true, "team_id": slack.team_id.as_deref().unwrap_or("T1")}),
            ),
            ("users.info", Some(info)) => Ok(info),
            _ => Err("slack_fake".into()),
        }
    }
    async fn slack_respond(&self, url: &str, body: &Value) -> Result<(), String> {
        self.slack
            .borrow_mut()
            .replies
            .push((url.to_owned(), body.clone()));
        Ok(())
    }
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), NoEntropy> {
        for b in bytes.iter_mut() {
            self.random.set(self.random.get().wrapping_add(1));
            *b = self.random.get();
        }
        Ok(())
    }
    async fn rate_limit(
        &self,
        _: &str,
        _: u64,
        _: u32,
    ) -> Result<(), runbook_api::backends::Limited> {
        Ok(())
    }
    async fn execution_owner(
        &self,
        _: &OrganizationId,
        _: &ExecutionId,
    ) -> Result<Option<TenantScope>, StoreError> {
        Ok(None)
    }
    async fn execution_scope(
        &self,
        _: &SecretHash,
    ) -> Result<Option<TenantScope>, CredentialError> {
        Ok(None)
    }
    async fn token_issue_replay(
        &self,
        _: &OrganizationId,
        _: &str,
        _: &str,
        _: &Value,
    ) -> Result<Option<IssueOutcome>, CredentialError> {
        Ok(None)
    }
    async fn token_issue(&self, issue: &TokenIssue) -> Result<IssueOutcome, CredentialError> {
        use runbook_core::credentials::CredentialStore;
        self.credentials.insert(issue.record.clone()).await?;
        self.issued_tokens
            .borrow_mut()
            .push((issue.record.clone(), issue.metadata.clone()));
        Ok(IssueOutcome::Issued)
    }
    async fn token_insert_issued(
        &self,
        _: &CredentialRecord,
        _: &TokenMetadata,
    ) -> Result<(), CredentialError> {
        Err(CredentialError::Backend("fake.tokens"))
    }
    async fn token_metadata(
        &self,
        _: &OrganizationId,
        id: &runbook_model::TokenId,
    ) -> Result<TokenMetadata, CredentialError> {
        Ok(self
            .issued_tokens
            .borrow()
            .iter()
            .find(|(record, _)| &record.id == id)
            .map(|(_, metadata)| metadata.clone())
            .unwrap_or_default())
    }
    async fn tokens_of_org(
        &self,
        _: &OrganizationId,
    ) -> Result<Vec<(CredentialRecord, TokenMetadata)>, CredentialError> {
        Ok(self.issued_tokens.borrow().clone())
    }
    async fn device_code_by_device_hash(
        &self,
        _: &SecretHash,
    ) -> Result<Option<DeviceCodeRecord>, CredentialError> {
        Ok(None)
    }
    async fn active_orgs_of_user(&self, _: &UserId) -> Result<Vec<OrganizationId>, DirectoryError> {
        Ok(vec![])
    }
    async fn expired_step_run(
        &self,
        _: &OrganizationId,
        _: &StepRunId,
    ) -> Result<Option<(ExecutionId, ProjectId)>, StoreError> {
        Ok(None)
    }
    async fn memory_project(
        &self,
        _: &OrganizationId,
        _: &MemoryId,
    ) -> Result<Option<ProjectId>, StoreError> {
        Ok(None)
    }
    async fn runbook_project(
        &self,
        _: &OrganizationId,
        _: &RunbookId,
    ) -> Result<Option<ProjectId>, StoreError> {
        Ok(None)
    }
    async fn me_tasks(
        &self,
        _: &MeTasksScope,
        _: &InboxFilters,
        _: Option<&str>,
        _: usize,
    ) -> Result<MeTasksPage, StoreError> {
        Err(StoreError::Backend("fake.me_tasks"))
    }
}

/// An API request as a host hands it over.
pub fn request(
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Value,
) -> runbook_api::Request {
    runbook_api::Request {
        method: method.into(),
        path: path.into(),
        query: Default::default(),
        body,
        credential: bearer.map(|b| runbook_api::Credential::Bearer(b.into())),
        idempotency_key: None,
        request_id: "req_test".into(),
        new_id: "00000000000000000000000001".into(),
        if_match: None,
        client_ip: None,
    }
}

/// Drive a router future on this thread (the fakes never wait).
pub fn run<F: std::future::Future>(future: F) -> F::Output {
    runbook_mcp::backend::block_on(future)
}
