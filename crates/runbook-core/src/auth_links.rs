//! Sign-in identity links and invitations (PRD §14 "One owner", §18; D45).
//! T0 freezes the records and trait; AUTH2 implements them with
//! `0027_auth_links_invitations_device`.
//!
//! Better Auth authenticates; the directory stays the only source of users
//! and roles. A session names an `auth_user_id`, which this store maps to a
//! directory user. Registration is allowed only for an email a directory
//! user already holds as verified, or one a pending invitation names.
use crate::directory::{DirectoryError, OrgRole};
use runbook_model::{InvitationId, OrganizationId, ProjectId, ProjectRole, UserId};
use serde::{Deserialize, Serialize};
use std::future::Future;

/// Default invitation lifetime (AUTH2 may make it configurable).
pub const INVITATION_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// One Better Auth account mapped to one directory user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthLink {
    /// Better Auth's user ID (opaque, from the auth service).
    pub auth_user_id: String,
    pub user_id: UserId,
    pub org_id: OrganizationId,
    /// The verified email the link was made with, normalized by `email_key`.
    pub email: String,
    pub linked_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvitationState {
    Pending,
    Accepted,
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationRecord {
    pub id: InvitationId,
    pub org_id: OrganizationId,
    /// Normalized by `directory::email_key`.
    pub email: String,
    /// Applied on acceptance (D45).
    pub role: OrgRole,
    /// Project roles granted on acceptance; may be empty.
    pub projects: Vec<(ProjectId, ProjectRole)>,
    pub invited_by: UserId,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub state: InvitationState,
    pub accepted_by: Option<UserId>,
    /// Acceptance or revocation time.
    pub closed_at_ms: Option<i64>,
    /// The invitee's name, for the account an acceptance creates.
    pub name: Option<String>,
}

/// First use of a session (D45): record the link and, with an invitation,
/// consume it and apply its roles, all in one directory commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRequest {
    pub auth_user_id: String,
    /// Verified by the auth service; unverified Google emails never get here.
    pub verified_email: String,
    /// The directory user the email resolves to, or a new host-supplied ID
    /// when an invitation creates the user.
    pub user_id: UserId,
    /// Display name for a user the invitation creates.
    pub name: String,
    pub invitation: Option<InvitationId>,
    pub now_ms: i64,
}

/// Directory-level storage for links and invitations. Mutations are scoped
/// by an explicit organization; hosts authorize them first (owner or admin
/// for invitations). The two global reads exist because a session and a
/// registration attempt name no organization; like a token hash lookup, they
/// never grant access by themselves. Futures need not be Send.
pub trait AuthLinkStore {
    /// Global key read: the link for a Better Auth user, if any.
    fn auth_link(
        &self,
        auth_user_id: &str,
    ) -> impl Future<Output = Result<Option<AuthLink>, DirectoryError>>;
    /// MCP2 re-check 1: the Better Auth users linked to a directory user (a
    /// person's own connected apps). Sorted.
    fn auth_users_of(
        &self,
        user_id: &UserId,
    ) -> impl Future<Output = Result<Vec<String>, DirectoryError>>;
    /// Only the links established in this organization, sorted by auth user ID.
    fn auth_links_in_org(
        &self,
        org: &OrganizationId,
    ) -> impl Future<Output = Result<Vec<AuthLink>, DirectoryError>>;
    /// Links eligible for organization-owner recovery. Requires active
    /// membership here, no membership elsewhere and no foreign auth link.
    fn owner_reset_auth_users(
        &self,
        org: &OrganizationId,
        user: &UserId,
    ) -> impl Future<Output = Result<Vec<String>, DirectoryError>>;
    /// Global read for `can-register`: pending invitations naming this email
    /// that have not expired at `now_ms`.
    fn invitations_for_email(
        &self,
        email: &str,
        now_ms: i64,
    ) -> impl Future<Output = Result<Vec<InvitationRecord>, DirectoryError>>;
    /// One directory commit (D45). Binds the verified email, creates the user
    /// when an invitation requires it, applies and consumes the invitation and
    /// records the link. Linking the same pair again returns the existing
    /// link; an `auth_user_id` linked to another user is `Duplicate`; an
    /// expired, revoked, accepted or foreign invitation is
    /// `Invalid("invitation")`.
    fn link_identity(
        &self,
        org: &OrganizationId,
        request: LinkRequest,
    ) -> impl Future<Output = Result<AuthLink, DirectoryError>>;
    /// `Duplicate` when a pending, unexpired invitation already names the email.
    fn create_invitation(
        &self,
        org: &OrganizationId,
        invitation: InvitationRecord,
    ) -> impl Future<Output = Result<(), DirectoryError>>;
    /// Every invitation of the organization, newest first.
    fn list_invitations(
        &self,
        org: &OrganizationId,
    ) -> impl Future<Output = Result<Vec<InvitationRecord>, DirectoryError>>;
    /// Pending to revoked. True only for the first revocation.
    fn revoke_invitation(
        &self,
        org: &OrganizationId,
        id: &InvitationId,
        now_ms: i64,
    ) -> impl Future<Output = Result<bool, DirectoryError>>;
}

fn org_rank(role: OrgRole) -> u8 {
    match role {
        OrgRole::Member => 0,
        OrgRole::Admin => 1,
        OrgRole::Owner => 2,
    }
}
fn project_rank(role: ProjectRole) -> u8 {
    match role {
        ProjectRole::Viewer => 0,
        ProjectRole::Operator => 1,
        ProjectRole::Editor => 2,
        ProjectRole::Admin => 3,
    }
}

/// What the adapter read before planning a link, inside its transaction.
pub struct LinkState<'a> {
    /// The current link for `request.auth_user_id`, if any.
    pub existing: Option<&'a AuthLink>,
    /// The named invitation, if the request names one and it exists.
    pub invitation: Option<&'a InvitationRecord>,
    /// The directory user holding the verified email, if any.
    pub email_owner: Option<&'a UserId>,
    /// The organization's directory as of the locked revision.
    pub snapshot: &'a crate::directory::DirectorySnapshot,
    /// The global identity name of `request.user_id`, if the user exists in
    /// any organization (a new membership must reuse it).
    pub identity_name: Option<&'a str>,
}

/// The outcome of [`plan_link`]: either the link already exists, or one
/// directory commit plus the link row and (with an invitation) its acceptance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkPlan {
    Existing(AuthLink),
    Commit {
        mutations: Vec<crate::directory::DirectoryMutation>,
        link: AuthLink,
        accept: Option<InvitationId>,
    },
}

/// D45 linking rules, shared by every adapter so they agree exactly:
/// - the same `(auth_user_id, user_id, org)` again returns the existing link;
///   an `auth_user_id` linked elsewhere is `Duplicate`;
/// - the request's user must be the one the verified email resolves to;
/// - with an invitation (pending, unexpired, this org, this email) the user
///   joins with the invited role, or keeps a higher one, and gains each
///   invited project role unless they already hold a higher one;
/// - without one, the email must already belong to an active member here.
pub fn plan_link(
    org: &OrganizationId,
    request: &LinkRequest,
    state: LinkState<'_>,
) -> Result<LinkPlan, DirectoryError> {
    use crate::directory::{DirectoryMutation, UserRecord, email_key};
    if let Some(existing) = state.existing {
        return if existing.user_id == request.user_id && &existing.org_id == org {
            Ok(LinkPlan::Existing(existing.clone()))
        } else {
            Err(DirectoryError::Duplicate)
        };
    }
    if request.auth_user_id.trim().is_empty() || request.auth_user_id.len() > 255 {
        return Err(DirectoryError::Invalid("auth_user_id"));
    }
    let email = email_key(&request.verified_email)?;
    if state
        .email_owner
        .is_some_and(|owner| owner != &request.user_id)
    {
        return Err(DirectoryError::Invalid("user"));
    }
    let member = state.snapshot.users.get(&request.user_id);
    let mut mutations = Vec::new();
    let accept = match &request.invitation {
        Some(id) => {
            let invitation = state
                .invitation
                .filter(|i| {
                    &i.id == id
                        && &i.org_id == org
                        && i.state == InvitationState::Pending
                        && i.expires_at_ms > request.now_ms
                        && i.email == email
                })
                .ok_or(DirectoryError::Invalid("invitation"))?;
            match member {
                None => {
                    // An existing identity keeps its name; else the name the
                    // inviter gave, else the sign-in's (Google's, or the
                    // email's first part).
                    let name = state
                        .identity_name
                        .map(str::to_owned)
                        .or_else(|| {
                            invitation
                                .name
                                .as_deref()
                                .map(str::trim)
                                .filter(|n| !n.is_empty())
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| request.name.trim().to_owned());
                    mutations.push(DirectoryMutation::CreateUser(UserRecord {
                        id: request.user_id.clone(),
                        name,
                        active: true,
                        role: invitation.role,
                    }));
                }
                Some(user) => {
                    if !user.active {
                        return Err(DirectoryError::Invalid("user inactive"));
                    }
                    if org_rank(invitation.role) > org_rank(user.role) {
                        mutations.push(DirectoryMutation::SetOrgRole {
                            user_id: request.user_id.clone(),
                            role: invitation.role,
                        });
                    }
                }
            }
            for (project_id, role) in &invitation.projects {
                let held = state
                    .snapshot
                    .project_members
                    .get(&(project_id.clone(), request.user_id.clone()));
                if held.is_none_or(|held| project_rank(*role) > project_rank(*held)) {
                    mutations.push(DirectoryMutation::GrantProjectRole {
                        project_id: project_id.clone(),
                        user_id: request.user_id.clone(),
                        role: *role,
                    });
                }
            }
            Some(id.clone())
        }
        None => {
            if state.email_owner.is_none() || !member.is_some_and(|u| u.active) {
                return Err(DirectoryError::Invalid("invitation"));
            }
            None
        }
    };
    // Idempotent for a bound email; it also makes the link a directory commit.
    mutations.push(DirectoryMutation::BindVerifiedEmail {
        user_id: request.user_id.clone(),
        email: email.clone(),
    });
    Ok(LinkPlan::Commit {
        mutations,
        link: AuthLink {
            auth_user_id: request.auth_user_id.clone(),
            user_id: request.user_id.clone(),
            org_id: org.clone(),
            email,
            linked_at_ms: request.now_ms,
        },
        accept,
    })
}

/// Checks an invitation before `create_invitation` stores it; adapters share it.
pub fn validate_invitation(
    org: &OrganizationId,
    invitation: &InvitationRecord,
    snapshot: &crate::directory::DirectorySnapshot,
) -> Result<InvitationRecord, DirectoryError> {
    if &invitation.org_id != org || invitation.state != InvitationState::Pending {
        return Err(DirectoryError::Invalid("invitation"));
    }
    if invitation.expires_at_ms <= invitation.created_at_ms {
        return Err(DirectoryError::Invalid("expiry"));
    }
    if invitation
        .projects
        .iter()
        .any(|(p, _)| !snapshot.projects.contains_key(p))
    {
        return Err(DirectoryError::NotFound);
    }
    let mut out = invitation.clone();
    out.email = crate::directory::email_key(&invitation.email)?;
    out.accepted_by = None;
    out.closed_at_ms = None;
    Ok(out)
}

/// Newest first, ties by ID descending, for `list_invitations`.
pub fn sort_invitations(list: &mut [InvitationRecord]) {
    list.sort_by(|a, b| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then_with(|| b.id.as_str().cmp(a.id.as_str()))
    });
}

/// D46 session principal: the record a linked browser session authorizes
/// as. It is a human personal credential for the linked user, valid until the
/// session expires, carrying the scopes the user's roles allow (never more):
/// every scope some project role of theirs permits, `tokens:manage` (people
/// manage their own tokens, D17), and `project:admin` for an org owner or
/// admin (organization administration). It is never an agent. `None` when
/// the user is not an active member. The token ID is derived from the
/// session ID and names no stored credential.
pub fn session_credential(
    snapshot: &crate::directory::DirectorySnapshot,
    user_id: &UserId,
    session_id: &str,
    expires_at_ms: i64,
) -> Option<crate::credentials::CredentialRecord> {
    use crate::credentials::{
        ActorKind, CredentialPrincipal, CredentialRecord, Scope, TokenKind, secret_hash,
    };
    let user = snapshot.users.get(user_id).filter(|u| u.active)?;
    let mut scopes = std::collections::BTreeSet::from([Scope::TokensManage]);
    let org_admin = matches!(user.role, OrgRole::Owner | OrgRole::Admin);
    for scope in Scope::ALL {
        // An org owner or admin is admin of every project (`project_role`).
        if org_admin
            || snapshot.project_members.iter().any(|((_, member), role)| {
                member == user_id && crate::identity::role_allows(*role, scope.project_action())
            })
        {
            scopes.insert(scope);
        }
    }
    // R0-2: every member reads the runner (the runner narrows to their
    // projects); org owners and admins administer it.
    scopes.insert(Scope::RunnerRead);
    if org_admin {
        scopes.insert(Scope::RunnerAdmin);
    }
    let digest = secret_hash(&format!("session:{session_id}"));
    Some(CredentialRecord {
        id: session_token_id(digest.as_bytes()),
        kind: TokenKind::Personal,
        hash: digest,
        principal: CredentialPrincipal::User {
            user_id: user_id.clone(),
            actor: ActorKind::Human,
        },
        org_id: snapshot.org_id.clone(),
        scopes,
        projects: None,
        expires_at: Some(expires_at_ms),
        revoked_at: None,
        last_used_at: None,
    })
}

/// Whether a record is a browser or OAuth session's (built per request by
/// [`session_credential`], never stored), rather than a stored token. Its ID
/// is derived from its hash, which no stored token's random ID matches.
pub fn is_session_record(record: &crate::credentials::CredentialRecord) -> bool {
    record.kind == crate::credentials::TokenKind::Personal
        && record.id == session_token_id(record.hash.as_bytes())
}

/// A `tok_` ID (Crockford base32, first character 0-7) from digest bytes.
fn session_token_id(digest: &[u8; 32]) -> runbook_model::TokenId {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut value = u128::from_be_bytes(digest[..16].try_into().expect("16 bytes"));
    let mut out = [b'0'; 26];
    for b in out.iter_mut().rev() {
        *b = ALPHABET[(value & 31) as usize];
        value >>= 5;
    }
    // 26 characters carry 130 bits; the first holds 3 and stays in 0-7.
    out[0] = ALPHABET[(digest[16] & 7) as usize];
    format!("tok_{}", std::str::from_utf8(&out).expect("ASCII"))
        .parse()
        .expect("valid token ID")
}
