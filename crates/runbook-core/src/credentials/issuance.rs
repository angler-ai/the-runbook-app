//! B4.1/C2 token management rules for `/v1/tokens`: who may manage, how a
//! new token narrows the calling credential, and what a caller may list or
//! revoke. Pure and portable; the host supplies time, IDs and entropy.
//!
//! - Only an active user's credential holding `tokens:manage` manages
//!   tokens. Service and execution credentials never do (D17: people manage
//!   tokens).
//! - Only human-kind PATs manage tokens; an agent-kind token never carries
//!   `tokens:manage` (not inherited, refused if requested).
//! - A token can only narrow its issuer: its scopes, projects and (for a
//!   personal token) expiry never exceed the calling credential. A service token's role never exceeds the caller's own
//!   project role.
//! - Tokens are issued only to the caller. There is no API impersonation.
//! - Tokens are independent records: revoking the token that minted another
//!   leaves the minted one valid (revoke each; the issuer is recorded as a
//!   user, not a parent token).
use super::{
    ActorKind, AuthError, CredentialPrincipal, CredentialRecord, Scope, SecretHash, TokenKind,
};
use crate::directory::{DirectorySnapshot, OrgRole};
use runbook_model::{
    CreateTokenRequest, ManagedTokenKind, ProjectId, ProjectRole, TokenId, TokenView, UserId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// PRD §9: personal access tokens last at most one year.
pub const MAX_PERSONAL_LIFETIME_MS: i64 = 365 * 86_400_000;
/// Clients compute expiry from their own clock; allow this much skew.
pub const CLOCK_SKEW_MS: i64 = 3_600_000;
/// Default personal token lifetime when the request names none.
pub const DEFAULT_PERSONAL_LIFETIME_MS: i64 = 90 * 86_400_000;
const MAX_NAME_CHARS: usize = 100;

/// Management metadata stored beside the credential record. Bootstrap and
/// operator-inserted credentials have none (`name` empty, times unknown).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenMetadata {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub created_by: Option<UserId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueError {
    /// 422 `request.invalid`: malformed or contradictory request.
    Invalid(&'static str),
    /// 403 `auth.forbidden`: another user, or a service token without org
    /// owner/admin authority.
    Forbidden,
    /// 403 `tokens.exceeds_caller`: the request would widen the caller.
    ExceedsCaller(&'static str),
    /// 404 `resource.not_found`: unknown or invisible project.
    NotFound,
}

/// The authenticated manager: always a user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manager {
    pub user_id: UserId,
    pub actor: ActorKind,
    pub org_role: OrgRole,
    /// The managing credential's project narrowing; `None` is org-wide.
    pub projects: Option<std::collections::BTreeSet<ProjectId>>,
}
impl Manager {
    /// Human org owners and admins manage service tokens and others' tokens
    /// (D17), but only through an org-wide credential: a project-narrowed
    /// PAT never carries org powers, as `authorize_organization` also
    /// refuses narrowed credentials (F8).
    pub fn org_admin(&self) -> bool {
        self.actor == ActorKind::Human
            && self.projects.is_none()
            && matches!(self.org_role, OrgRole::Owner | OrgRole::Admin)
    }
    /// D20 replay identity, matching the directory routes.
    pub fn principal(&self) -> String {
        match self.actor {
            ActorKind::Human => format!("human:{}", self.user_id),
            ActorKind::Agent => format!("agent:{}", self.user_id),
        }
    }
}

/// `MissingScope` is reported as 403 `auth.scope_missing`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManageError {
    Auth(AuthError),
    MissingScope,
}

pub fn authorize_management(
    r: &CredentialRecord,
    s: &DirectorySnapshot,
    now: i64,
) -> Result<Manager, ManageError> {
    r.validate()
        .map_err(|_| ManageError::Auth(AuthError::InvalidCredential))?;
    if r.revoked_at.is_some() {
        return Err(ManageError::Auth(AuthError::Revoked));
    }
    if r.expires_at.is_some_and(|t| now >= t) {
        return Err(ManageError::Auth(AuthError::Expired));
    }
    if r.org_id != s.org_id {
        return Err(ManageError::Auth(AuthError::Forbidden));
    }
    // D17: people manage tokens. Agent-kind PATs never do, whatever their scopes.
    let CredentialPrincipal::User {
        user_id,
        actor: actor @ ActorKind::Human,
    } = &r.principal
    else {
        return Err(ManageError::Auth(AuthError::Forbidden));
    };
    let user = s
        .users
        .get(user_id)
        .filter(|u| u.active)
        .ok_or(ManageError::Auth(AuthError::Forbidden))?;
    if !r.scopes.contains(&Scope::TokensManage) {
        return Err(ManageError::MissingScope);
    }
    Ok(Manager {
        user_id: user_id.clone(),
        actor: *actor,
        org_role: user.role,
        projects: r.projects.clone(),
    })
}

fn rank(role: ProjectRole) -> u8 {
    match role {
        ProjectRole::Viewer => 0,
        ProjectRole::Operator => 1,
        ProjectRole::Editor => 2,
        ProjectRole::Admin => 3,
    }
}

/// A project ID or slug in the caller's organization.
fn resolve(s: &DirectorySnapshot, reference: &str) -> Result<ProjectId, IssueError> {
    if let Ok(id) = reference.parse::<ProjectId>()
        && s.projects.contains_key(&id)
    {
        return Ok(id);
    }
    s.projects
        .values()
        .find(|p| p.slug == reference)
        .map(|p| p.id.clone())
        .ok_or(IssueError::NotFound)
}

fn parse_scopes(names: &[String]) -> Result<BTreeSet<Scope>, IssueError> {
    if names.is_empty() {
        return Err(IssueError::Invalid("scopes"));
    }
    names
        .iter()
        .map(|n| {
            serde_json::from_value::<Scope>(serde_json::Value::String(n.clone()))
                .map_err(|_| IssueError::Invalid("scopes"))
        })
        .collect()
}

/// Plan the new credential. `caller` is the record `manager` was
/// authorized from; nothing is written here.
pub fn plan_issue(
    caller: &CredentialRecord,
    manager: &Manager,
    s: &DirectorySnapshot,
    now: i64,
    request: &CreateTokenRequest,
    id: TokenId,
    hash: SecretHash,
) -> Result<(CredentialRecord, TokenMetadata), IssueError> {
    if request
        .user_id
        .as_ref()
        .is_some_and(|u| u != &manager.user_id)
    {
        return Err(IssueError::Forbidden);
    }
    let kind = request.kind.unwrap_or(ManagedTokenKind::Personal);
    let name = match &request.name {
        Some(name) => {
            let trimmed = name.trim();
            if trimmed.is_empty()
                || trimmed.chars().count() > MAX_NAME_CHARS
                || trimmed.chars().any(char::is_control)
            {
                return Err(IssueError::Invalid("name"));
            }
            trimmed.to_owned()
        }
        None => String::new(),
    };
    let agent = kind == ManagedTokenKind::Service
        || request.actor_kind == Some(runbook_model::ActorKind::Agent);
    let scopes = match &request.scopes {
        Some(names) => parse_scopes(names)?,
        // An agent-kind token never inherits tokens:manage (D17).
        None if kind == ManagedTokenKind::Personal => caller
            .scopes
            .iter()
            .filter(|scope| !agent || **scope != Scope::TokensManage)
            .copied()
            .collect(),
        None => return Err(IssueError::Invalid("scopes")),
    };
    if scopes.is_empty()
        || (agent && kind == ManagedTokenKind::Personal && scopes.contains(&Scope::TokensManage))
    {
        return Err(IssueError::Invalid("scopes"));
    }
    if !scopes.is_subset(&caller.scopes) {
        return Err(IssueError::ExceedsCaller("scopes"));
    }
    let projects = match &request.projects {
        Some(refs) => {
            if refs.is_empty() {
                return Err(IssueError::Invalid("projects"));
            }
            let mut resolved = BTreeSet::new();
            for reference in refs {
                let id = resolve(s, reference)?;
                if caller.projects.as_ref().is_some_and(|p| !p.contains(&id)) {
                    return Err(IssueError::ExceedsCaller("projects"));
                }
                if !super::directory_project_visible(caller, s, &id) {
                    return Err(IssueError::NotFound);
                }
                resolved.insert(id);
            }
            Some(resolved)
        }
        None => caller.projects.clone(),
    };
    if request.expires_at_ms.is_some_and(|t| t <= now) {
        return Err(IssueError::Invalid("expires_at_ms"));
    }
    let meta = TokenMetadata {
        name,
        created_at: Some(now),
        created_by: Some(manager.user_id.clone()),
    };
    let record = match kind {
        ManagedTokenKind::Personal => {
            if request.role.is_some() {
                return Err(IssueError::Invalid("role"));
            }
            // Managers are always human (`authorize_management`).
            let actor = if agent {
                ActorKind::Agent
            } else {
                ActorKind::Human
            };
            let limit = now.saturating_add(MAX_PERSONAL_LIFETIME_MS);
            let expires_at = match request.expires_at_ms {
                Some(t) => {
                    if t > limit.saturating_add(CLOCK_SKEW_MS) {
                        return Err(IssueError::Invalid("expires_at_ms"));
                    }
                    if caller.expires_at.is_some_and(|c| t > c) {
                        return Err(IssueError::ExceedsCaller("expires_at_ms"));
                    }
                    t
                }
                None => {
                    let default = now.saturating_add(DEFAULT_PERSONAL_LIFETIME_MS);
                    caller.expires_at.map_or(default, |c| c.min(default))
                }
            };
            CredentialRecord {
                id,
                kind: TokenKind::Personal,
                hash,
                principal: CredentialPrincipal::User {
                    user_id: manager.user_id.clone(),
                    actor,
                },
                org_id: s.org_id.clone(),
                scopes,
                projects,
                expires_at: Some(expires_at),
                revoked_at: None,
                last_used_at: None,
            }
        }
        ManagedTokenKind::Service => {
            if !manager.org_admin() {
                return Err(IssueError::Forbidden);
            }
            if request.actor_kind == Some(runbook_model::ActorKind::Human) {
                return Err(IssueError::Invalid("actor_kind"));
            }
            let role = request.role.ok_or(IssueError::Invalid("role"))?;
            let project = match &projects {
                Some(p) if p.len() == 1 && request.projects.is_some() => {
                    p.iter()
                        .next()
                        .cloned()
                        .ok_or(IssueError::Invalid("projects"))?
                }
                _ => return Err(IssueError::Invalid("projects")),
            };
            let own = s
                .project_role(&project, &manager.user_id)
                .ok_or(IssueError::ExceedsCaller("role"))?;
            if rank(role) > rank(own) {
                return Err(IssueError::ExceedsCaller("role"));
            }
            CredentialRecord {
                id,
                kind: TokenKind::Service,
                hash,
                principal: CredentialPrincipal::Service {
                    project_id: project.clone(),
                    role,
                },
                org_id: s.org_id.clone(),
                scopes,
                projects: Some([project].into()),
                expires_at: request.expires_at_ms,
                revoked_at: None,
                last_used_at: None,
            }
        }
    };
    record.validate().map_err(|_| IssueError::Invalid("kind"))?;
    Ok((record, meta))
}

/// What `GET /v1/tokens` shows: the caller's own live personal tokens
/// (human and agent kind), plus the organization's service tokens for org
/// owners and admins.
pub fn listed(manager: &Manager, record: &CredentialRecord) -> bool {
    if record.revoked_at.is_some() {
        return false;
    }
    match &record.principal {
        CredentialPrincipal::User { user_id, .. } => user_id == &manager.user_id,
        CredentialPrincipal::Service { .. } => manager.org_admin(),
    }
}

fn org_rank(role: OrgRole) -> u8 {
    match role {
        OrgRole::Member => 0,
        OrgRole::Admin => 1,
        OrgRole::Owner => 2,
    }
}

/// `DELETE /v1/tokens/{id}`: anything the caller can list, and (D17) a
/// personal token of another user whose org role is not above the
/// caller's: owners revoke anyone's, admins revoke members' and other
/// admins' but never an owner's. A user missing from the directory ranks
/// as a member.
pub fn may_revoke(manager: &Manager, s: &DirectorySnapshot, record: &CredentialRecord) -> bool {
    if listed(manager, record) {
        return true;
    }
    let CredentialPrincipal::User { user_id, .. } = &record.principal else {
        return false;
    };
    let owner_role = s.users.get(user_id).map_or(OrgRole::Member, |u| u.role);
    manager.org_admin()
        && record.revoked_at.is_none()
        && org_rank(manager.org_role) >= org_rank(owner_role)
}

fn scope_name(scope: &Scope) -> String {
    serde_json::to_value(scope)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Metadata only: never the secret or its hash.
pub fn view(record: &CredentialRecord, meta: &TokenMetadata) -> TokenView {
    let (kind, user_id, actor_kind, project_id, role) = match &record.principal {
        CredentialPrincipal::User { user_id, actor } => (
            ManagedTokenKind::Personal,
            Some(user_id.clone()),
            Some(match actor {
                ActorKind::Human => runbook_model::ActorKind::Human,
                ActorKind::Agent => runbook_model::ActorKind::Agent,
            }),
            None,
            None,
        ),
        CredentialPrincipal::Service { project_id, role } => (
            ManagedTokenKind::Service,
            None,
            None,
            Some(project_id.clone()),
            Some(*role),
        ),
    };
    TokenView {
        id: record.id.clone(),
        kind,
        name: meta.name.clone(),
        scopes: record.scopes.iter().map(scope_name).collect(),
        projects: record
            .projects
            .as_ref()
            .map(|p| p.iter().cloned().collect()),
        user_id,
        actor_kind,
        project_id,
        role,
        created_by: meta.created_by.clone(),
        created_at_ms: meta.created_at,
        expires_at_ms: record.expires_at,
        last_used_at_ms: record.last_used_at,
    }
}
