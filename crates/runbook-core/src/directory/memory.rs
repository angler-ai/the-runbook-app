//! Reference adapter: one synchronous staged transaction, no await under borrow.
use super::*;
use std::{cell::RefCell, rc::Rc};
mod auth_links;
mod portable;
type ReplayKey = (OrganizationId, String, String, String);
type ReplayEntry = (
    serde_json::Value,
    crate::directory_operations::StoredResponse,
    i64,
);
#[derive(Clone, Default)]
struct Database {
    variables: BTreeMap<ProjectId, crate::directory_operations::VariablesRecord>,
    replays: BTreeMap<ReplayKey, ReplayEntry>,
    time_ms: i64,
    orgs: BTreeMap<OrganizationId, DirectorySnapshot>,
    users: BTreeMap<UserId, UserIdentity>,
    projects: BTreeMap<ProjectId, OrganizationId>,
    teams: BTreeMap<TeamId, OrganizationId>,
    emails: BTreeMap<String, UserId>,
    auth_links: BTreeMap<String, crate::auth_links::AuthLink>,
    invitations: BTreeMap<runbook_model::InvitationId, crate::auth_links::InvitationRecord>,
    pictures: BTreeMap<crate::pictures::PictureOwner, crate::pictures::Picture>,
}
#[derive(Clone, Default)]
pub struct MemoryDirectory {
    database: Rc<RefCell<Database>>,
}

impl Database {
    fn apply(&mut self, c: DirectoryCommit) -> Result<CommitResult, DirectoryError> {
        let old = self.orgs.get(&c.org_id).map_or(0, |s| s.revision);
        if old != c.expected_revision {
            return Ok(CommitResult::Conflict);
        }
        if c.mutations.is_empty() {
            return Err(DirectoryError::Invalid("empty commit"));
        }
        let revision = old.checked_add(1).ok_or(DirectoryError::RevisionOverflow)?;
        let had_owner = self.orgs.get(&c.org_id).is_some_and(|s| {
            s.users
                .values()
                .any(|u| u.active && u.role == OrgRole::Owner)
        });
        for mutation in c.mutations {
            if let DirectoryMutation::CreateOrganization { title } = &mutation {
                if self.orgs.contains_key(&c.org_id) {
                    return Err(DirectoryError::Duplicate);
                }
                if title.trim().is_empty() {
                    return Err(DirectoryError::Invalid("title"));
                }
                self.orgs.insert(
                    c.org_id.clone(),
                    DirectorySnapshot {
                        org_id: c.org_id.clone(),
                        revision: 0,
                        organization: OrganizationRecord {
                            id: c.org_id.clone(),
                            title: title.clone(),
                        },
                        users: BTreeMap::new(),
                        projects: BTreeMap::new(),
                        teams: BTreeMap::new(),
                        project_members: BTreeMap::new(),
                        team_members: BTreeSet::new(),
                        archived_projects: Default::default(),
                    },
                );
                continue;
            }
            let s = self
                .orgs
                .get_mut(&c.org_id)
                .ok_or(DirectoryError::NotFound)?;
            match mutation {
                DirectoryMutation::CreateOrganization { .. } => unreachable!(),
                DirectoryMutation::CreateUser(u) => {
                    if u.name.trim().is_empty() {
                        return Err(DirectoryError::Invalid("user name"));
                    }
                    if s.users.contains_key(&u.id) {
                        return Err(DirectoryError::Duplicate);
                    }
                    let identity = UserIdentity {
                        id: u.id.clone(),
                        name: u.name.clone(),
                    };
                    if self
                        .users
                        .get(&u.id)
                        .is_some_and(|existing| existing != &identity)
                    {
                        return Err(DirectoryError::Duplicate);
                    }
                    self.users.insert(u.id.clone(), identity);
                    s.users.insert(u.id.clone(), u);
                }
                DirectoryMutation::CreateProject(p) => {
                    if p.org_id != c.org_id {
                        return Err(DirectoryError::Invalid("organization mismatch"));
                    }
                    if !slug(&p.slug) || p.title.trim().is_empty() {
                        return Err(DirectoryError::Invalid("project"));
                    }
                    if self.projects.contains_key(&p.id)
                        || s.projects.values().any(|x| x.slug == p.slug)
                    {
                        return Err(DirectoryError::Duplicate);
                    }
                    self.projects.insert(p.id.clone(), c.org_id.clone());
                    s.projects.insert(p.id.clone(), p);
                }
                DirectoryMutation::CreateTeam(t) => {
                    if t.org_id != c.org_id {
                        return Err(DirectoryError::Invalid("organization mismatch"));
                    }
                    if !slug(&t.slug) || t.title.trim().is_empty() {
                        return Err(DirectoryError::Invalid("team"));
                    }
                    if self.teams.contains_key(&t.id) || s.teams.values().any(|x| x.slug == t.slug)
                    {
                        return Err(DirectoryError::Duplicate);
                    }
                    self.teams.insert(t.id.clone(), c.org_id.clone());
                    s.teams.insert(t.id.clone(), t);
                }
                DirectoryMutation::SetOrgRole { user_id, role } => {
                    s.users
                        .get_mut(&user_id)
                        .ok_or(DirectoryError::NotFound)?
                        .role = role;
                }
                DirectoryMutation::SetUserActive { user_id, active } => {
                    s.users
                        .get_mut(&user_id)
                        .ok_or(DirectoryError::NotFound)?
                        .active = active
                }
                DirectoryMutation::SetProjectArchived {
                    project_id,
                    archived,
                } => {
                    if !s.projects.contains_key(&project_id) {
                        return Err(DirectoryError::NotFound);
                    }
                    if archived {
                        s.archived_projects.insert(project_id);
                    } else {
                        s.archived_projects.remove(&project_id);
                    }
                }
                DirectoryMutation::SetProjectTitle { project_id, title } => {
                    if title.trim().is_empty() {
                        return Err(DirectoryError::Invalid("project"));
                    }
                    s.projects
                        .get_mut(&project_id)
                        .ok_or(DirectoryError::NotFound)?
                        .title = title;
                }
                DirectoryMutation::SetUserName { user_id, name } => {
                    if name.trim().is_empty() {
                        return Err(DirectoryError::Invalid("user name"));
                    }
                    s.users
                        .get_mut(&user_id)
                        .ok_or(DirectoryError::NotFound)?
                        .name = name.clone();
                    if let Some(identity) = self.users.get_mut(&user_id) {
                        identity.name = name.clone();
                    }
                    for (org, other) in self.orgs.iter_mut() {
                        if org != &c.org_id
                            && let Some(user) = other.users.get_mut(&user_id)
                        {
                            user.name = name.clone();
                        }
                    }
                }
                DirectoryMutation::GrantProjectRole {
                    project_id,
                    user_id,
                    role,
                } => {
                    if !s.projects.contains_key(&project_id) || !s.users.contains_key(&user_id) {
                        return Err(DirectoryError::NotFound);
                    }
                    s.project_members.insert((project_id, user_id), role);
                }
                DirectoryMutation::RevokeProjectRole {
                    project_id,
                    user_id,
                } => {
                    if !s.projects.contains_key(&project_id) || !s.users.contains_key(&user_id) {
                        return Err(DirectoryError::NotFound);
                    }
                    s.project_members.remove(&(project_id, user_id));
                }
                DirectoryMutation::AddTeamMember { team_id, user_id } => {
                    if !s.teams.contains_key(&team_id) || !s.users.contains_key(&user_id) {
                        return Err(DirectoryError::NotFound);
                    }
                    s.team_members.insert((team_id, user_id));
                }
                DirectoryMutation::RemoveTeamMember { team_id, user_id } => {
                    if !s.teams.contains_key(&team_id) || !s.users.contains_key(&user_id) {
                        return Err(DirectoryError::NotFound);
                    }
                    s.team_members.remove(&(team_id, user_id));
                }
                DirectoryMutation::BindVerifiedEmail { user_id, email } => {
                    if !s.users.contains_key(&user_id) {
                        return Err(DirectoryError::NotFound);
                    }
                    let key = email_key(&email)?;
                    if self.emails.get(&key).is_some_and(|id| id != &user_id) {
                        return Err(DirectoryError::Duplicate);
                    }
                    self.emails.insert(key, user_id);
                }
            }
        }
        if had_owner
            && !self
                .orgs
                .get(&c.org_id)
                .ok_or(DirectoryError::NotFound)?
                .users
                .values()
                .any(|u| u.active && u.role == OrgRole::Owner)
        {
            return Err(DirectoryError::Invalid("last owner"));
        }
        self.orgs
            .get_mut(&c.org_id)
            .ok_or(DirectoryError::NotFound)?
            .revision = revision;
        Ok(CommitResult::Committed(revision))
    }
}
impl crate::pictures::PictureStore for MemoryDirectory {
    async fn picture(
        &self,
        owner: &crate::pictures::PictureOwner,
    ) -> Result<Option<crate::pictures::Picture>, DirectoryError> {
        Ok(self.database.borrow().pictures.get(owner).cloned())
    }
    async fn set_picture(
        &self,
        owner: &crate::pictures::PictureOwner,
        picture: Option<crate::pictures::Picture>,
    ) -> Result<(), DirectoryError> {
        let mut db = self.database.borrow_mut();
        match picture {
            Some(picture) => {
                db.pictures.insert(owner.clone(), picture);
            }
            None => {
                db.pictures.remove(owner);
            }
        }
        Ok(())
    }
}
impl DirectoryStore for MemoryDirectory {
    async fn snapshot(&self, org_id: &OrganizationId) -> Result<DirectorySnapshot, DirectoryError> {
        self.database
            .borrow()
            .orgs
            .get(org_id)
            .cloned()
            .ok_or(DirectoryError::NotFound)
    }
    async fn commit(&self, c: DirectoryCommit) -> Result<CommitResult, DirectoryError> {
        let mut db = self.database.borrow_mut();
        let mut staged = db.clone();
        let result = staged.apply(c)?;
        if matches!(result, CommitResult::Committed(_)) {
            *db = staged
        }
        Ok(result)
    }
    async fn user_by_id(&self, id: &UserId) -> Result<Option<UserIdentity>, DirectoryError> {
        Ok(self.database.borrow().users.get(id).cloned())
    }
    async fn user_by_verified_email(
        &self,
        email: &str,
    ) -> Result<Option<UserIdentity>, DirectoryError> {
        let key = email_key(email)?;
        let db = self.database.borrow();
        Ok(db.emails.get(&key).and_then(|id| db.users.get(id)).cloned())
    }
    async fn verified_emails(
        &self,
        org_id: &OrganizationId,
    ) -> Result<std::collections::BTreeMap<UserId, String>, DirectoryError> {
        let db = self.database.borrow();
        let org = db.orgs.get(org_id).ok_or(DirectoryError::NotFound)?;
        Ok(db
            .emails
            .iter()
            .filter(|(_, user_id)| org.users.contains_key(*user_id))
            .map(|(email, user_id)| (user_id.clone(), email.clone()))
            .collect())
    }
    async fn org_for_project(
        &self,
        id: &ProjectId,
    ) -> Result<Option<OrganizationId>, DirectoryError> {
        Ok(self.database.borrow().projects.get(id).cloned())
    }
}

impl crate::directory_operations::DirectoryOperations for MemoryDirectory {
    async fn lookup_replay(
        &self,
        org: &OrganizationId,
        expected_revision: u64,
        replay: &crate::directory_operations::RequestReplay,
    ) -> Result<crate::directory_operations::ReplayResult, DirectoryError> {
        use crate::directory_operations::ReplayResult;
        let db = self.database.borrow();
        if db.orgs.get(org).ok_or(DirectoryError::NotFound)?.revision != expected_revision {
            return Ok(ReplayResult::Conflict);
        }
        let key = (
            org.clone(),
            replay.principal.clone(),
            replay.operation.clone(),
            replay.key.clone(),
        );
        if let Some((payload, response, expiry)) = db.replays.get(&key)
            && *expiry > db.time_ms
        {
            return Ok(if payload == &replay.payload {
                ReplayResult::Applied(response.clone())
            } else {
                ReplayResult::KeyReused
            });
        }
        Ok(ReplayResult::Missing)
    }

    async fn current_time_ms(&self) -> Result<i64, DirectoryError> {
        Ok(self.database.borrow().time_ms)
    }
    async fn project_variables(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
    ) -> Result<crate::directory_operations::VariablesRecord, DirectoryError> {
        let db = self.database.borrow();
        if db.projects.get(project) != Some(org) {
            return Err(DirectoryError::NotFound);
        }
        Ok(db.variables.get(project).cloned().unwrap_or_default())
    }
    async fn apply_operation(
        &self,
        operation: crate::directory_operations::DirectoryOperation,
    ) -> Result<crate::directory_operations::OperationResult, DirectoryError> {
        use crate::directory_operations::*;
        let mut db = self.database.borrow_mut();
        let mut staged = db.clone();
        let old = staged
            .orgs
            .get(&operation.commit.org_id)
            .ok_or(DirectoryError::NotFound)?
            .revision;
        if old != operation.commit.expected_revision {
            return Ok(OperationResult::Conflict);
        }
        let key = operation.replay.as_ref().map(|r| {
            (
                operation.commit.org_id.clone(),
                r.principal.clone(),
                r.operation.clone(),
                r.key.clone(),
            )
        });
        if let Some((key, r)) = key.as_ref().zip(operation.replay.as_ref())
            && let Some((payload, response, expiry)) = staged.replays.get(key)
            && *expiry > staged.time_ms
        {
            return Ok(if payload == &r.payload {
                OperationResult::Applied(response.clone())
            } else {
                OperationResult::KeyReused
            });
        }
        let org = operation.commit.org_id.clone();
        if operation.commit.mutations.is_empty() {
            if operation.variables.is_none() {
                return Err(DirectoryError::Invalid("empty operation"));
            }
            staged
                .orgs
                .get_mut(&org)
                .ok_or(DirectoryError::NotFound)?
                .revision = old.checked_add(1).ok_or(DirectoryError::RevisionOverflow)?;
        } else if staged.apply(operation.commit)? == CommitResult::Conflict {
            return Ok(OperationResult::Conflict);
        }
        if let Some(v) = operation.variables {
            if staged.projects.get(&v.project_id) != Some(&org) {
                return Err(DirectoryError::NotFound);
            }
            let old = staged
                .variables
                .get(&v.project_id)
                .map_or(0, |v| v.revision);
            if old != v.expected_revision {
                return Ok(OperationResult::VariablesConflict);
            }
            staged.variables.insert(
                v.project_id,
                VariablesRecord {
                    revision: old.checked_add(1).ok_or(DirectoryError::RevisionOverflow)?,
                    variables: v.variables,
                },
            );
        }
        staged
            .replays
            .retain(|_, (_, _, expiry)| *expiry > staged.time_ms);
        if let Some((key, r)) = key.zip(operation.replay) {
            staged.replays.insert(
                key,
                (
                    r.payload,
                    operation.response.clone(),
                    staged.time_ms.saturating_add(86_400_000),
                ),
            );
        }
        *db = staged;
        Ok(OperationResult::Applied(operation.response))
    }
}

impl MemoryDirectory {
    /// Deterministic host-supplied clock for the reference adapter.
    pub fn set_time_ms(&self, now: i64) {
        self.database.borrow_mut().time_ms = now;
    }
}
