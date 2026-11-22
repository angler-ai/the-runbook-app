//! R0-2: the runner registry (runner design §6.4, §12.1-2).
//!
//! One runner install (control plane) per organization, with its groups
//! and runners. The install registers with a single-use bootstrap token an
//! org admin created for its URL, and gets three secrets: a directory
//! token (`runner:directory`, runner-api), a release token
//! (`runner:release`, vault) and a join token. Each runner joins with the
//! join token and gets its own claim token (`runner:claim`), which claims
//! only steps for agents in its group. Every secret is 32 random bytes and
//! stored as its SHA-256 only. Runner secrets never pass through person
//! authorization: the API's runner allow-list (`runner_access.rs`) answers
//! them on a fixed set of routes.
use crate::{
    StoreError,
    credentials::{
        ActorKind, AuthError, CredentialPrincipal, CredentialRecord, Scope, SecretHash, secret_hash,
    },
    directory::{DirectorySnapshot, OrgRole},
    webhooks::{TargetPolicy, parse_target},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_model::*;
use std::future::Future;

/// A runner install without a heartbeat for this long is `offline`.
pub const HEARTBEAT_STALE_MS: i64 = 90_000;
/// A bootstrap token registers an install within this long.
pub const BOOTSTRAP_TTL_MS: i64 = 24 * 3600 * 1000;

/// The kinds of runner secret, each with its own prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerSecretKind {
    Bootstrap,
    Join,
    Directory,
    Claim,
    Release,
}
impl RunnerSecretKind {
    pub const ALL: [Self; 5] = [
        Self::Bootstrap,
        Self::Join,
        Self::Directory,
        Self::Claim,
        Self::Release,
    ];
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Bootstrap => "trb_rbs_",
            Self::Join => "trb_rjn_",
            Self::Directory => "trb_rdi_",
            Self::Claim => "trb_rcl_",
            Self::Release => "trb_rrl_",
        }
    }
}
pub fn encode_runner_secret(kind: RunnerSecretKind, random32: [u8; 32]) -> String {
    format!("{}{}", kind.prefix(), URL_SAFE_NO_PAD.encode(random32))
}
/// The kind of a well-formed runner secret.
pub fn runner_secret_kind(secret: &str) -> Option<RunnerSecretKind> {
    let kind = RunnerSecretKind::ALL
        .into_iter()
        .find(|k| secret.starts_with(k.prefix()))?;
    let suffix = &secret[kind.prefix().len()..];
    (suffix.len() == 43 && URL_SAFE_NO_PAD.decode(suffix).is_ok_and(|b| b.len() == 32))
        .then_some(kind)
}

/// What a runner token may do (its core scope).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerPurpose {
    /// `runner:directory`: heartbeats, the agent directory, waiting
    /// reasons, events and the ready listing (runner-api).
    Directory,
    /// `runner:claim`: claim and work steps of its group's agents (worker).
    Claim,
    /// `runner:release`: release grants (vault).
    Release,
    /// The join token: adds runners to the install.
    Join,
}
/// An authenticated runner token. Not a [`crate::credentials::Principal`]:
/// it never passes through person authorization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerPrincipal {
    pub org_id: OrganizationId,
    pub install_id: RunnerInstallId,
    /// Set for claim tokens.
    pub runner_id: Option<RunnerId>,
    /// Set for claim tokens: the runner's group.
    pub group_id: Option<RunnerGroupId>,
    pub purpose: RunnerPurpose,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunnerError {
    /// 409 `runner.already_registered`.
    AlreadyRegistered,
    /// 401 `auth.invalid_credential`: unknown, used or expired token.
    InvalidToken,
    /// 403 `runner.url_mismatch`: registration from another URL than the
    /// bootstrap token was made for.
    UrlMismatch,
    /// 422 `runner.contract_unsupported`.
    ContractUnsupported,
    /// 404 `resource.not_found`.
    NotFound,
    /// 409 with a code.
    Conflict(&'static str),
    /// 403 with a code.
    Forbidden(&'static str),
    /// 422 with a code.
    Invalid(&'static str),
    /// 501 `runner.unsupported`: this host has no runner registry yet.
    Unsupported,
    Store(StoreError),
}
impl RunnerError {
    pub fn status(&self) -> (u16, &'static str) {
        match self {
            Self::AlreadyRegistered => (409, "runner.already_registered"),
            Self::InvalidToken => (401, "auth.invalid_credential"),
            Self::UrlMismatch => (403, "runner.url_mismatch"),
            Self::ContractUnsupported => (422, "runner.contract_unsupported"),
            Self::NotFound => (404, "resource.not_found"),
            Self::Conflict(code) => (409, code),
            Self::Forbidden(code) => (403, code),
            Self::Invalid(code) => (422, code),
            Self::Unsupported => (501, "runner.unsupported"),
            Self::Store(_) => (503, "storage.unavailable"),
        }
    }
}
impl From<StoreError> for RunnerError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// A stored install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallRecord {
    pub id: RunnerInstallId,
    pub org_id: OrganizationId,
    pub url: String,
    pub version: String,
    pub contract_version: u32,
    pub vault_identity_key: Option<String>,
    pub created_at_ms: i64,
    pub last_heartbeat_at_ms: Option<i64>,
}
/// What registration stores: the install and its secrets' hashes.
#[derive(Clone, Debug)]
pub struct NewInstall {
    pub id: RunnerInstallId,
    pub url: String,
    pub version: String,
    pub contract_version: u32,
    pub vault_identity_key: Option<String>,
    pub directory: SecretHash,
    pub release: SecretHash,
    pub join: SecretHash,
}
#[derive(Clone, Debug)]
pub struct NewRunner {
    pub id: RunnerId,
    pub name: String,
    pub group_id: RunnerGroupId,
    pub group_name: String,
    pub version: String,
    pub claim: SecretHash,
}

/// The registry's storage. Times are the host's epoch milliseconds.
pub trait RunnerStore {
    /// Store a bootstrap token for `org`'s runner at `url`; earlier unused
    /// ones stop working.
    fn create_bootstrap(
        &self,
        org: &OrganizationId,
        url: &str,
        token: &SecretHash,
        now_ms: i64,
    ) -> impl Future<Output = Result<(), RunnerError>>;
    /// Use a bootstrap token once: refuse a second install for its org
    /// (`AlreadyRegistered`) or another URL (`UrlMismatch`).
    fn register(
        &self,
        bootstrap: &SecretHash,
        install: NewInstall,
        now_ms: i64,
    ) -> impl Future<Output = Result<InstallRecord, RunnerError>>;
    /// Add a runner and its group with the join token of `install`. A
    /// runner joining again under the same name in the same group keeps
    /// its ID and gets the new claim token (the old one stops working).
    fn join(
        &self,
        install: &RunnerInstallId,
        join: &SecretHash,
        runner: NewRunner,
        now_ms: i64,
    ) -> impl Future<Output = Result<RunnerId, RunnerError>>;
    /// The runner token with this hash, of any purpose.
    fn authenticate(
        &self,
        token: &SecretHash,
    ) -> impl Future<Output = Result<Option<RunnerPrincipal>, RunnerError>>;
    fn heartbeat(
        &self,
        install: &RunnerInstallId,
        heartbeat: &RunnerHeartbeatRequest,
        now_ms: i64,
    ) -> impl Future<Output = Result<(), RunnerError>>;
    /// The org's install, groups (with allowed projects) and runners.
    fn status(
        &self,
        org: &OrganizationId,
        now_ms: i64,
    ) -> impl Future<Output = Result<RunnerStatusView, RunnerError>>;
    /// Append a key change to the installation's audit history.
    fn set_vault_identity(
        &self,
        _org: &OrganizationId,
        _owner: &UserId,
        _key: &str,
        _now_ms: i64,
    ) -> impl Future<Output = Result<(), RunnerError>> {
        async { Err(RunnerError::Unsupported) }
    }
    /// `NotFound` when the group is not the org's.
    fn set_group_projects(
        &self,
        org: &OrganizationId,
        group: &RunnerGroupId,
        projects: &[ProjectId],
    ) -> impl Future<Output = Result<(), RunnerError>>;
    /// The install of an org, if any.
    fn install_of(
        &self,
        org: &OrganizationId,
    ) -> impl Future<Output = Result<Option<InstallRecord>, RunnerError>>;
    /// R0-4: the group of one of the install's runners.
    fn runner_group(
        &self,
        install: &RunnerInstallId,
        runner: &RunnerId,
    ) -> impl Future<Output = Result<Option<RunnerGroupId>, RunnerError>>;
    /// R0-4: ready steps of the install's agents in `group`, oldest first.
    fn runner_supports_decisions(
        &self,
        install: &RunnerInstallId,
        runner: &RunnerId,
        since: i64,
    ) -> impl Future<Output = Result<bool, RunnerError>>;
    fn ready_steps(
        &self,
        install: &RunnerInstallId,
        group: &RunnerGroupId,
        project: Option<&ProjectId>,
        after: Option<(i64, &StepRunId)>,
        limit: u64,
        decision_since: Option<i64>,
    ) -> impl Future<Output = Result<Vec<ReadyStepView>, RunnerError>>;
}

/// A host without a runner registry yet (the Cloudflare edition until R0b):
/// every call is `runner.unsupported`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unsupported;
impl RunnerStore for Unsupported {
    async fn create_bootstrap(
        &self,
        _: &OrganizationId,
        _: &str,
        _: &SecretHash,
        _: i64,
    ) -> Result<(), RunnerError> {
        Err(RunnerError::Unsupported)
    }
    async fn register(
        &self,
        _: &SecretHash,
        _: NewInstall,
        _: i64,
    ) -> Result<InstallRecord, RunnerError> {
        Err(RunnerError::Unsupported)
    }
    async fn join(
        &self,
        _: &RunnerInstallId,
        _: &SecretHash,
        _: NewRunner,
        _: i64,
    ) -> Result<RunnerId, RunnerError> {
        Err(RunnerError::Unsupported)
    }
    async fn authenticate(&self, _: &SecretHash) -> Result<Option<RunnerPrincipal>, RunnerError> {
        Ok(None)
    }
    async fn heartbeat(
        &self,
        _: &RunnerInstallId,
        _: &RunnerHeartbeatRequest,
        _: i64,
    ) -> Result<(), RunnerError> {
        Err(RunnerError::Unsupported)
    }
    async fn status(&self, _: &OrganizationId, _: i64) -> Result<RunnerStatusView, RunnerError> {
        Ok(no_install())
    }
    async fn set_group_projects(
        &self,
        _: &OrganizationId,
        _: &RunnerGroupId,
        _: &[ProjectId],
    ) -> Result<(), RunnerError> {
        Err(RunnerError::Unsupported)
    }
    async fn install_of(&self, _: &OrganizationId) -> Result<Option<InstallRecord>, RunnerError> {
        Ok(None)
    }
    async fn runner_group(
        &self,
        _: &RunnerInstallId,
        _: &RunnerId,
    ) -> Result<Option<RunnerGroupId>, RunnerError> {
        Ok(None)
    }
    async fn runner_supports_decisions(
        &self,
        _: &RunnerInstallId,
        _: &RunnerId,
        _: i64,
    ) -> Result<bool, RunnerError> {
        Ok(false)
    }
    async fn ready_steps(
        &self,
        _: &RunnerInstallId,
        _: &RunnerGroupId,
        _: Option<&ProjectId>,
        _: Option<(i64, &StepRunId)>,
        _: u64,
        _: Option<i64>,
    ) -> Result<Vec<ReadyStepView>, RunnerError> {
        Err(RunnerError::Unsupported)
    }
}

/// The status of an org with no install.
pub fn no_install() -> RunnerStatusView {
    RunnerStatusView {
        install_id: None,
        status: RunnerInstallStatus::None,
        version: None,
        contract_version: None,
        vault_identity_key: None,
        groups: vec![],
        runners: vec![],
        last_heartbeat_at: None,
    }
}
/// Online while the last heartbeat is younger than [`HEARTBEAT_STALE_MS`].
pub fn install_status(last_heartbeat_at_ms: Option<i64>, now_ms: i64) -> RunnerInstallStatus {
    match last_heartbeat_at_ms {
        Some(at) if now_ms - at < HEARTBEAT_STALE_MS => RunnerInstallStatus::Online,
        _ => RunnerInstallStatus::Offline,
    }
}

/// The person behind a runner request (`runner:read` or `runner:admin`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerActor {
    pub user_id: UserId,
    pub actor: ActorKind,
    pub org_role: OrgRole,
    /// The runner scopes the credential holds.
    pub scopes: Vec<Scope>,
}

/// Authorize a person's (or their agent's) credential for an org-level
/// runner action. `runner:read` needs an active member; `runner:admin`
/// needs an active org owner or admin, a human principal and an
/// unrestricted project list. The credential must hold `scope` itself:
/// roles never imply scopes (`runner:read` does not follow from
/// `runner:admin` either).
pub fn authorize_person(
    r: &CredentialRecord,
    s: &DirectorySnapshot,
    now: i64,
    scope: Scope,
) -> Result<RunnerActor, AuthError> {
    r.validate().map_err(|_| AuthError::InvalidCredential)?;
    if r.revoked_at.is_some() {
        return Err(AuthError::Revoked);
    }
    if r.expires_at.is_some_and(|t| now >= t) {
        return Err(AuthError::Expired);
    }
    let CredentialPrincipal::User { user_id, actor } = &r.principal else {
        return Err(AuthError::Forbidden);
    };
    if r.org_id != s.org_id || !r.scopes.contains(&scope) {
        return Err(AuthError::Forbidden);
    }
    let user = s
        .users
        .get(user_id)
        .filter(|u| u.active)
        .ok_or(AuthError::Forbidden)?;
    if scope == Scope::RunnerAdmin
        && (!matches!(user.role, OrgRole::Owner | OrgRole::Admin)
            || *actor != ActorKind::Human
            || r.projects.is_some())
    {
        return Err(AuthError::Forbidden);
    }
    Ok(RunnerActor {
        user_id: user_id.clone(),
        actor: *actor,
        org_role: user.role,
        scopes: [Scope::RunnerRead, Scope::RunnerAdmin]
            .into_iter()
            .filter(|s| r.scopes.contains(s))
            .collect(),
    })
}

/// R0-5: the person (or their agent) behind a proxied runner request: an
/// active member whose credential holds `runner:read` or `runner:admin`.
/// The runner authorizes each action from the assertion; the core only
/// vouches for who is asking and with which runner scopes.
pub fn authorize_proxy(
    r: &CredentialRecord,
    s: &DirectorySnapshot,
    now: i64,
) -> Result<RunnerActor, AuthError> {
    let scope = Scope::RUNNER
        .into_iter()
        .find(|scope| r.scopes.contains(scope))
        .ok_or(AuthError::Forbidden)?;
    let mut actor = authorize_person(r, s, now, Scope::RunnerRead).or_else(|e| {
        if scope == Scope::RunnerAdmin && e == AuthError::Forbidden {
            // runner:admin alone: the same membership checks as a read.
            let mut read_only = r.clone();
            read_only.scopes.insert(Scope::RunnerRead);
            authorize_person(&read_only, s, now, Scope::RunnerRead)
        } else {
            Err(e)
        }
    })?;
    actor.scopes = Scope::RUNNER
        .into_iter()
        .filter(|s| r.scopes.contains(s))
        .collect();
    Ok(actor)
}

/// The roles a proxied request asserts for `actor`: the org role and
/// project roles from the directory, narrowed to the credential's projects
/// when it is restricted to some. A project-restricted credential asserts
/// only those projects and the org role `member`, whatever the person's
/// org role: it never acts org-wide.
pub fn asserted_roles(
    r: &CredentialRecord,
    s: &DirectorySnapshot,
    actor: &RunnerActor,
) -> (OrgRole, std::collections::BTreeMap<ProjectId, ProjectRole>) {
    let roles = s
        .projects
        .keys()
        .filter(|p| {
            r.projects
                .as_ref()
                .is_none_or(|allowed| allowed.contains(*p))
        })
        .filter_map(|p| Some((p.clone(), s.project_role(p, &actor.user_id)?)))
        .collect();
    let org_role = if r.projects.is_some() {
        OrgRole::Member
    } else {
        actor.org_role
    };
    (org_role, roles)
}

/// Host names that are cloud metadata services whatever they resolve to.
const METADATA_HOSTS: &[&str] = &[
    "metadata.google.internal",
    "metadata.goog",
    "metadata",
    "instance-data",
    "instance-data.ec2.internal",
];

/// A runner URL under the webhook target rules (`webhooks/target.rs`):
/// https only unless the operator allows private networks
/// (`RUNBOOK_RUNNER_PRIVATE_NETWORKS=true`), never link-local or a
/// metadata host, no credentials, no fragment. Hosts resolve and vet the
/// address again on every proxied request.
pub fn validate_url(url: &str, allow_private: bool) -> Result<(), &'static str> {
    let policy = TargetPolicy {
        allow_private,
        allow_link_local: false,
    };
    let target = parse_target(url, policy).map_err(|code| match code {
        "webhooks.target_forbidden" | "webhooks.url_https_required" => "runner.url_forbidden",
        _ => "runner.url_invalid",
    })?;
    let host = target.host.trim_end_matches('.');
    if METADATA_HOSTS.contains(&host) {
        return Err("runner.url_forbidden");
    }
    if url.contains('?') {
        return Err("runner.url_invalid");
    }
    Ok(())
}

/// The hash a runner secret is stored under.
pub fn token_hash(secret: &str) -> SecretHash {
    secret_hash(secret)
}
