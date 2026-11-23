//! Portable hash-only PAT/service credentials. Hosts supply entropy and time.
//! Execution/refresh encoding is pinned here; their authorization belongs to B7.
use crate::{
    directory::DirectorySnapshot,
    identity::{ProjectAction, effective_permission},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_model::{OrganizationId, ProjectId, ProjectRole, TokenId, UserId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt, future::Future};
pub mod issuance;
pub mod memory;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    Personal,
    Service,
    Execution,
    Refresh,
}
impl TokenKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Personal => "trb_pat_",
            Self::Service => "trb_svc_",
            Self::Execution => "trb_exe_",
            Self::Refresh => "trb_exr_",
        }
    }
}
/// Transient entropy, never serialized or printed by Debug.
pub struct SecretBytes([u8; 32]);
impl SecretBytes {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes([REDACTED])")
    }
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SecretHash([u8; 32]);
impl SecretHash {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
impl fmt::Debug for SecretHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretHash([REDACTED])")
    }
}
pub fn encode_secret(kind: TokenKind, random32: [u8; 32]) -> String {
    format!("{}{}", kind.prefix(), URL_SAFE_NO_PAD.encode(random32))
}
pub fn parse_secret(secret: &str) -> Result<(TokenKind, SecretBytes), AuthError> {
    let kind = [
        TokenKind::Personal,
        TokenKind::Service,
        TokenKind::Execution,
        TokenKind::Refresh,
    ]
    .into_iter()
    .find(|k| secret.starts_with(k.prefix()))
    .ok_or(AuthError::InvalidCredential)?;
    let suffix = &secret[kind.prefix().len()..];
    if suffix.len() != 43 {
        return Err(AuthError::InvalidCredential);
    }
    let bytes: [u8; 32] = URL_SAFE_NO_PAD
        .decode(suffix)
        .map_err(|_| AuthError::InvalidCredential)?
        .try_into()
        .map_err(|_| AuthError::InvalidCredential)?;
    Ok((kind, SecretBytes(bytes)))
}
pub fn secret_hash(secret: &str) -> SecretHash {
    SecretHash(Sha256::digest(secret.as_bytes()).into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialPrincipal {
    User {
        user_id: UserId,
        actor: ActorKind,
    },
    /// A service credential is its own identity, without a human sponsor. Its
    /// project role is an explicit issuance grant; revoke/reissue to change it.
    Service {
        project_id: ProjectId,
        role: ProjectRole,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    Human {
        user_id: UserId,
    },
    Agent {
        sponsor: UserId,
    },
    Service {
        token_id: TokenId,
        project_id: ProjectId,
    },
}
/// The action is also the exact required token scope; broad roles never imply scopes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scope {
    #[serde(rename = "runbooks:read")]
    RunbooksRead,
    #[serde(rename = "runbooks:write")]
    RunbooksWrite,
    #[serde(rename = "runbooks:publish")]
    RunbooksPublish,
    #[serde(rename = "executions:read")]
    ExecutionsRead,
    #[serde(rename = "executions:start")]
    ExecutionsStart,
    #[serde(rename = "executions:act")]
    ExecutionsAct,
    #[serde(rename = "executions:admin")]
    ExecutionsAdmin,
    #[serde(rename = "skills:read")]
    SkillsRead,
    #[serde(rename = "skills:write")]
    SkillsWrite,
    #[serde(rename = "tokens:manage")]
    TokensManage,
    #[serde(rename = "webhooks:manage")]
    WebhooksManage,
    #[serde(rename = "project:admin")]
    ProjectAdmin,
    /// R0-2: read the org's runner and its agents through the proxy.
    #[serde(rename = "runner:read")]
    RunnerRead,
    /// R0-2: administer the org's runner (registration, groups, and
    /// through the proxy the runner's configuration).
    #[serde(rename = "runner:admin")]
    RunnerAdmin,
}
impl Scope {
    /// Every PRD §9 scope, in declaration order.
    pub const ALL: [Scope; 12] = [
        Self::RunbooksRead,
        Self::RunbooksWrite,
        Self::RunbooksPublish,
        Self::ExecutionsRead,
        Self::ExecutionsStart,
        Self::ExecutionsAct,
        Self::ExecutionsAdmin,
        Self::SkillsRead,
        Self::SkillsWrite,
        Self::TokensManage,
        Self::WebhooksManage,
        Self::ProjectAdmin,
    ];
    /// The runner scopes (runner design §11.4). Not in [`Self::ALL`]: they
    /// are granted to people's sessions by org role and to tokens only when
    /// chosen explicitly.
    pub const RUNNER: [Scope; 2] = [Self::RunnerRead, Self::RunnerAdmin];
    /// Browser sessions can reach project agent authoring through the proxy.
    /// The runner checks the target project's role for each operation.
    pub fn browser_runner_admin(role: ProjectRole) -> bool {
        matches!(role, ProjectRole::Admin | ProjectRole::Editor)
    }

    pub fn project_action(self) -> ProjectAction {
        match self {
            Self::RunbooksRead | Self::ExecutionsRead | Self::SkillsRead | Self::RunnerRead => {
                ProjectAction::Read
            }
            Self::RunnerAdmin => ProjectAction::Manage,
            Self::ExecutionsStart | Self::ExecutionsAct | Self::ExecutionsAdmin => {
                ProjectAction::Run
            }
            Self::RunbooksWrite | Self::SkillsWrite => ProjectAction::Write,
            Self::RunbooksPublish => ProjectAction::Publish,
            Self::TokensManage | Self::WebhooksManage | Self::ProjectAdmin => ProjectAction::Manage,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRecord {
    pub id: TokenId,
    pub kind: TokenKind,
    pub hash: SecretHash,
    pub principal: CredentialPrincipal,
    pub org_id: OrganizationId,
    pub scopes: BTreeSet<Scope>,
    /// None means all projects the principal may access; Some(empty) grants none.
    pub projects: Option<BTreeSet<ProjectId>>,
    /// UTC epoch milliseconds. None is permitted for non-expiring service tokens.
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub last_used_at: Option<i64>,
}
impl CredentialRecord {
    pub fn validate(&self) -> Result<(), CredentialError> {
        match (&self.kind, &self.principal) {
            (TokenKind::Personal, CredentialPrincipal::User { .. })
                if self.expires_at.is_some() =>
            {
                Ok(())
            }
            (TokenKind::Service, CredentialPrincipal::Service { .. }) => Ok(()),
            _ => Err(CredentialError::Invalid("kind or principal")),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    InvalidCredential,
    Expired,
    Revoked,
    Forbidden,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialError {
    Duplicate,
    Invalid(&'static str),
    Backend(&'static str),
}

pub fn authorize(
    r: &CredentialRecord,
    s: &DirectorySnapshot,
    now: i64,
    project: &ProjectId,
    action: Scope,
) -> Result<Principal, AuthError> {
    r.validate().map_err(|_| AuthError::InvalidCredential)?;
    if r.revoked_at.is_some() {
        return Err(AuthError::Revoked);
    }
    if r.expires_at.is_some_and(|expiry| now >= expiry) {
        return Err(AuthError::Expired);
    }
    if r.org_id != s.org_id
        || !s
            .projects
            .get(project)
            .is_some_and(|p| p.org_id == r.org_id)
    {
        return Err(AuthError::Forbidden);
    }
    // An archived project is read-only for everyone (2026-11-18).
    if s.archived_projects.contains(project)
        && !matches!(
            action,
            Scope::RunbooksRead | Scope::ExecutionsRead | Scope::SkillsRead
        )
    {
        return Err(AuthError::Forbidden);
    }
    let (role, principal) = match &r.principal {
        CredentialPrincipal::User { user_id, actor } => {
            if !s.users.get(user_id).is_some_and(|u| u.active) {
                return Err(AuthError::Forbidden);
            }
            let role = &s
                .project_role(project, user_id)
                .ok_or(AuthError::Forbidden)?;
            let principal = match actor {
                ActorKind::Human => Principal::Human {
                    user_id: user_id.clone(),
                },
                ActorKind::Agent => Principal::Agent {
                    sponsor: user_id.clone(),
                },
            };
            (*role, principal)
        }
        CredentialPrincipal::Service { project_id, role } => {
            if project_id != project {
                return Err(AuthError::Forbidden);
            }
            (
                *role,
                Principal::Service {
                    token_id: r.id.clone(),
                    project_id: project.clone(),
                },
            )
        }
    };
    let token_allows =
        r.scopes.contains(&action) && r.projects.as_ref().is_none_or(|p| p.contains(project));
    if !effective_permission(role, action.project_action(), token_allows) {
        return Err(AuthError::Forbidden);
    }
    Ok(principal)
}
/// Widen the subtraction so extreme host timestamps cannot wrap or panic.
pub fn should_touch_last_used(prev: Option<i64>, now: i64) -> bool {
    prev.is_none_or(|prev| i128::from(now) - i128::from(prev) >= 300_000)
}
/// Hash lookup is global because the secret carries no org. Mutations require
/// an explicit org and token ID; hosts authorize management before calling them.
pub trait CredentialStore {
    fn insert(&self, record: CredentialRecord)
    -> impl Future<Output = Result<(), CredentialError>>;
    fn find_by_hash(
        &self,
        hash: &SecretHash,
    ) -> impl Future<Output = Result<Option<CredentialRecord>, CredentialError>>;
    fn find_by_id(
        &self,
        id: &TokenId,
    ) -> impl Future<Output = Result<Option<CredentialRecord>, CredentialError>>;
    /// True only for the first revocation, preserving its timestamp on retries.
    fn revoke(
        &self,
        org: &OrganizationId,
        id: &TokenId,
        now: i64,
    ) -> impl Future<Output = Result<bool, CredentialError>>;
    /// True only if a row was updated. The throttle must be atomic in storage.
    fn touch_last_used(
        &self,
        org: &OrganizationId,
        id: &TokenId,
        now: i64,
    ) -> impl Future<Output = Result<bool, CredentialError>>;
}

/// Organization administration never follows from a project role. Narrowed
/// project credentials cannot perform organization-wide directory operations.
pub fn authorize_organization(
    r: &CredentialRecord,
    s: &DirectorySnapshot,
    now: i64,
    action: crate::identity::OrgAction,
) -> Result<Principal, AuthError> {
    r.validate().map_err(|_| AuthError::InvalidCredential)?;
    if r.revoked_at.is_some() {
        return Err(AuthError::Revoked);
    }
    if r.expires_at.is_some_and(|t| now >= t) {
        return Err(AuthError::Expired);
    }
    if r.org_id != s.org_id || r.projects.is_some() {
        return Err(AuthError::Forbidden);
    }
    let scope = if action == crate::identity::OrgAction::ManageTokens {
        Scope::TokensManage
    } else {
        Scope::ProjectAdmin
    };
    if !r.scopes.contains(&scope)
        || !s
            .users
            .values()
            .any(|u| u.active && u.role == crate::directory::OrgRole::Owner)
    {
        return Err(AuthError::Forbidden);
    }
    let CredentialPrincipal::User {
        user_id,
        actor: ActorKind::Human,
    } = &r.principal
    else {
        return Err(AuthError::Forbidden);
    };
    let user = s
        .users
        .get(user_id)
        .filter(|u| u.active)
        .ok_or(AuthError::Forbidden)?;
    if !crate::identity::org_action_allowed(user.role, action) {
        return Err(AuthError::Forbidden);
    }
    Ok(Principal::Human {
        user_id: user_id.clone(),
    })
}

/// D37 directory reads expose recipient identities, never grant execution authority.
pub fn authorize_directory_read(
    r: &CredentialRecord,
    s: &DirectorySnapshot,
    now: i64,
) -> Result<Principal, AuthError> {
    r.validate().map_err(|_| AuthError::InvalidCredential)?;
    if r.revoked_at.is_some() {
        return Err(AuthError::Revoked);
    }
    if r.expires_at.is_some_and(|t| now >= t) {
        return Err(AuthError::Expired);
    }
    if r.org_id != s.org_id
        || r.projects.as_ref().is_some_and(|p| p.is_empty())
        || ![
            Scope::ProjectAdmin,
            Scope::RunbooksRead,
            Scope::ExecutionsRead,
        ]
        .iter()
        .any(|scope| r.scopes.contains(scope))
    {
        return Err(AuthError::Forbidden);
    }
    match &r.principal {
        CredentialPrincipal::User { user_id, actor } => {
            if !s.users.get(user_id).is_some_and(|u| u.active) {
                return Err(AuthError::Forbidden);
            }
            Ok(match actor {
                ActorKind::Human => Principal::Human {
                    user_id: user_id.clone(),
                },
                ActorKind::Agent => Principal::Agent {
                    sponsor: user_id.clone(),
                },
            })
        }
        CredentialPrincipal::Service { project_id, .. } => {
            if !s.projects.contains_key(project_id)
                || r.projects.as_ref().is_some_and(|p| !p.contains(project_id))
            {
                return Err(AuthError::Forbidden);
            }
            Ok(Principal::Service {
                token_id: r.id.clone(),
                project_id: project_id.clone(),
            })
        }
    }
}
pub fn directory_project_visible(
    r: &CredentialRecord,
    s: &DirectorySnapshot,
    project: &ProjectId,
) -> bool {
    if !s.projects.contains_key(project)
        || r.projects.as_ref().is_some_and(|p| !p.contains(project))
    {
        return false;
    }
    match &r.principal {
        CredentialPrincipal::User { user_id, .. } => s.users.get(user_id).is_some_and(|u| {
            u.active
                && (crate::identity::org_action_allowed(
                    u.role,
                    crate::identity::OrgAction::ManageMemberships,
                ) || s
                    .project_members
                    .contains_key(&(project.clone(), user_id.clone())))
        }),
        CredentialPrincipal::Service { project_id, .. } => project_id == project,
    }
}
