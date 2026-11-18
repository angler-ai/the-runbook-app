//! Organization-local authorization facts. Global key reads never grant access.
use crate::CommitResult;
use runbook_model::{OrganizationId, ProjectId, ProjectRole, TeamId, UserId};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
};
pub mod memory;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationRecord {
    pub id: OrganizationId,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserIdentity {
    pub id: UserId,
    pub name: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgRole {
    Owner,
    Admin,
    Member,
}

/// Active here means active within this organization, not a browser session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRecord {
    pub id: UserId,
    pub name: String,
    pub active: bool,
    pub role: OrgRole,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: ProjectId,
    pub org_id: OrganizationId,
    pub slug: String,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamRecord {
    pub id: TeamId,
    pub org_id: OrganizationId,
    pub slug: String,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectorySnapshot {
    pub org_id: OrganizationId,
    pub revision: u64,
    pub organization: OrganizationRecord,
    pub users: BTreeMap<UserId, UserRecord>,
    pub projects: BTreeMap<ProjectId, ProjectRecord>,
    pub teams: BTreeMap<TeamId, TeamRecord>,
    pub project_members: BTreeMap<(ProjectId, UserId), ProjectRole>,
    pub team_members: BTreeSet<(TeamId, UserId)>,
}
impl DirectorySnapshot {
    /// The role `user` holds on `project` for access checks: an org owner or
    /// admin is admin of every project in the org without a grant (you,
    /// 2026-11-17); anyone else holds what they were granted. Who counts as
    /// a member (member lists, task fan-out, invitations) still reads
    /// `project_members` itself.
    pub fn project_role(&self, project: &ProjectId, user: &UserId) -> Option<ProjectRole> {
        let in_org = self
            .projects
            .get(project)
            .is_some_and(|p| p.org_id == self.org_id);
        let org_admin = self
            .users
            .get(user)
            .is_some_and(|u| u.active && matches!(u.role, OrgRole::Owner | OrgRole::Admin));
        if in_org && org_admin {
            return Some(ProjectRole::Admin);
        }
        self.project_members
            .get(&(project.clone(), user.clone()))
            .copied()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectoryMutation {
    CreateOrganization {
        title: String,
    },
    CreateUser(UserRecord),
    CreateProject(ProjectRecord),
    CreateTeam(TeamRecord),
    SetOrgRole {
        user_id: UserId,
        role: OrgRole,
    },
    SetUserActive {
        user_id: UserId,
        active: bool,
    },
    /// A project's display name (its slug stays).
    SetProjectTitle {
        project_id: ProjectId,
        title: String,
    },
    /// A person's display name: one identity, so every org they belong to
    /// shows the new name.
    SetUserName {
        user_id: UserId,
        name: String,
    },
    GrantProjectRole {
        project_id: ProjectId,
        user_id: UserId,
        role: ProjectRole,
    },
    RevokeProjectRole {
        project_id: ProjectId,
        user_id: UserId,
    },
    AddTeamMember {
        team_id: TeamId,
        user_id: UserId,
    },
    RemoveTeamMember {
        team_id: TeamId,
        user_id: UserId,
    },
    /// Only a trusted identity-verification boundary may submit this fact.
    BindVerifiedEmail {
        user_id: UserId,
        email: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryCommit {
    pub org_id: OrganizationId,
    pub expected_revision: u64,
    pub mutations: Vec<DirectoryMutation>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryError {
    NotFound,
    Duplicate,
    Invalid(&'static str),
    RevisionOverflow,
    /// Adapter infrastructure failure. The memory directory never produces it.
    Backend(&'static str),
}
/// Slug shape for project and team records, shared by every adapter.
pub fn slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && s.as_bytes()[0].is_ascii_lowercase()
        && !s.ends_with('-')
}
/// Normalized verified-email key, shared by every adapter: trimmed and
/// lowercased with full Unicode case mapping, as the auth service's
/// JavaScript `toLowerCase` does (sign-in gate fix 7), so a non-ASCII
/// address is one key however it was typed.
pub fn email_key(email: &str) -> Result<String, DirectoryError> {
    let e = email.trim().to_lowercase();
    let parts: Vec<_> = e.split('@').collect();
    if parts.len() != 2
        || parts.iter().any(|p| p.is_empty())
        || e.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(DirectoryError::Invalid("email"));
    }
    Ok(e)
}
pub trait DirectoryStore {
    fn snapshot(
        &self,
        org_id: &OrganizationId,
    ) -> impl Future<Output = Result<DirectorySnapshot, DirectoryError>>;
    fn commit(
        &self,
        commit: DirectoryCommit,
    ) -> impl Future<Output = Result<CommitResult, DirectoryError>>;
    fn user_by_id(
        &self,
        user_id: &UserId,
    ) -> impl Future<Output = Result<Option<UserIdentity>, DirectoryError>>;
    fn user_by_verified_email(
        &self,
        email: &str,
    ) -> impl Future<Output = Result<Option<UserIdentity>, DirectoryError>>;
    fn org_for_project(
        &self,
        project_id: &ProjectId,
    ) -> impl Future<Output = Result<Option<OrganizationId>, DirectoryError>>;
    /// NOTIFY1: the verified email of each of the org's members who has
    /// one, keyed by user. Org-scoped: bindings to people outside the org
    /// never appear.
    fn verified_emails(
        &self,
        org_id: &OrganizationId,
    ) -> impl Future<Output = Result<std::collections::BTreeMap<UserId, String>, DirectoryError>>;
}

#[cfg(test)]
mod email_key_tests {
    use super::email_key;
    #[test]
    fn email_keys_lowercase_all_of_unicode_like_javascript() {
        assert_eq!(email_key(" Ada@Example.TEST ").unwrap(), "ada@example.test");
        assert_eq!(
            email_key("Zoë.ÖLANDER@Exämple.TEST").unwrap(),
            "zoë.ölander@exämple.test"
        );
        // Word-final sigma, as String.prototype.toLowerCase maps it.
        assert_eq!(email_key("ΟΔΥΣΣΕΥΣ@x.test").unwrap(), "οδυσσευς@x.test");
        for bad in [
            "a@@b",
            "@b",
            "a@",
            "a b@c",
            "a\u{a0}b@c",
            "a\u{2028}@c",
            "a\u{7}@c",
        ] {
            assert!(email_key(bad).is_err(), "{bad:?}");
        }
    }
}
