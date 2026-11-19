//! B1 `DirectoryStore`, `DirectoryOperations` and `AuthLinkStore` on D1.
//!
//! Postgres locks the organization's revision row and applies mutations in
//! one transaction. D1 has no such lock, so a commit is planned and then
//! written as one guarded batch:
//! - one read batch (a consistent snapshot) loads the organization and the
//!   global facts the mutations touch (identity names, project and team IDs,
//!   verified-email owners);
//! - the mutations are checked against a working copy with the memory
//!   directory's exact semantics and error variants;
//! - the write batch starts with a guard that the revision is still the one
//!   read (or that the organization still does not exist), so a racing
//!   commit makes this one a `Conflict` and nothing of it lands. Global
//!   identity and email rows are written idempotently and then guarded for
//!   equality, and project/team primary keys refuse racing duplicates.
//!
//! Every write to an organization's directory tables bumps its revision
//! (links and invitation acceptance included), so "revision unchanged"
//! means "everything the plan read is unchanged".
use crate::sql::{Columns, D1, D1Error, Statement, guard, json, placeholders, prepare, uint};
use runbook_core::{
    CommitResult,
    auth_links::{
        AuthLink, AuthLinkStore, InvitationRecord, InvitationState, LinkPlan, LinkRequest,
        LinkState, plan_link, validate_invitation,
    },
    directory::{
        DirectoryCommit, DirectoryError, DirectoryMutation, DirectorySnapshot, DirectoryStore,
        OrgRole, OrganizationRecord, ProjectRecord, TeamRecord, UserIdentity, UserRecord,
        email_key, slug,
    },
    directory_operations::{
        DirectoryOperation, DirectoryOperations, OperationResult, ReplayResult, RequestReplay,
        VariablesRecord,
    },
};
use runbook_model::{InvitationId, OrganizationId, ProjectId, ProjectRole, UserId};
use std::collections::{BTreeMap, BTreeSet};

/// Directory handle over one D1 database. Clones share it.
#[derive(Clone)]
pub struct D1Directory<D: D1> {
    pub(crate) db: D,
    /// CF12 review round 1: resolves request replay responses kept in a
    /// project's blobs (`crate::replay`).
    replay_blobs: Option<std::rc::Rc<dyn crate::replay::ReplayBlobs>>,
}
impl<D: D1> D1Directory<D> {
    pub fn new(db: D) -> Self {
        Self {
            db,
            replay_blobs: None,
        }
    }
    /// This directory, reading replay responses kept in projects' blobs
    /// through `blobs`.
    pub fn with_replay_blobs(mut self, blobs: std::rc::Rc<dyn crate::replay::ReplayBlobs>) -> Self {
        self.replay_blobs = Some(blobs);
        self
    }
}

pub(crate) fn err(error: D1Error) -> DirectoryError {
    match error {
        D1Error::Unique => DirectoryError::Duplicate,
        D1Error::Guard(code) if code == "duplicate" => DirectoryError::Duplicate,
        D1Error::Guard(code) if code == "not_found" => DirectoryError::NotFound,
        D1Error::Guard(code) if code == "invitation" => DirectoryError::Invalid("invitation"),
        D1Error::Range => DirectoryError::Backend("d1.decode"),
        D1Error::Foreign => DirectoryError::NotFound,
        _ => DirectoryError::Backend("d1.directory"),
    }
}

pub(crate) fn org_role_label(role: OrgRole) -> &'static str {
    match role {
        OrgRole::Owner => "owner",
        OrgRole::Admin => "admin",
        OrgRole::Member => "member",
    }
}
pub(crate) fn org_role_of(text: &str) -> Result<OrgRole, DirectoryError> {
    match text {
        "owner" => Ok(OrgRole::Owner),
        "admin" => Ok(OrgRole::Admin),
        "member" => Ok(OrgRole::Member),
        _ => Err(DirectoryError::Backend("d1.invalid_org_role")),
    }
}
pub(crate) fn role_label(role: ProjectRole) -> &'static str {
    match role {
        ProjectRole::Viewer => "viewer",
        ProjectRole::Operator => "operator",
        ProjectRole::Editor => "editor",
        ProjectRole::Admin => "admin",
    }
}
pub(crate) fn role_of(text: &str) -> Result<ProjectRole, DirectoryError> {
    match text {
        "viewer" => Ok(ProjectRole::Viewer),
        "operator" => Ok(ProjectRole::Operator),
        "editor" => Ok(ProjectRole::Editor),
        "admin" => Ok(ProjectRole::Admin),
        _ => Err(DirectoryError::Backend("d1.invalid_role")),
    }
}

/// The snapshot reads, in batch order (one consistent read).
fn snapshot_reads(org: &str) -> Vec<Statement> {
    vec![
        prepare("SELECT title FROM organization WHERE org_id = ?").bind(org),
        prepare("SELECT revision FROM directory_revision WHERE org_id = ?").bind(org),
        prepare("SELECT user_id, name, active, org_role FROM org_user WHERE org_id = ?").bind(org),
        prepare("SELECT project_id, slug, title, archived FROM project WHERE org_id = ?").bind(org),
        prepare("SELECT team_id, slug, title FROM team WHERE org_id = ?").bind(org),
        prepare("SELECT project_id, user_id, role FROM project_member WHERE org_id = ?").bind(org),
        prepare("SELECT team_id, user_id FROM team_member WHERE org_id = ?").bind(org),
    ]
}
const SNAPSHOT_READS: usize = 7;

/// Decode the snapshot reads; `None` when the organization does not exist.
fn decode_snapshot(
    org_id: &OrganizationId,
    rows: &[crate::sql::Outcome],
) -> Result<Option<DirectorySnapshot>, DirectoryError> {
    let Some(title) = rows[0].rows.first() else {
        return Ok(None);
    };
    let title = title.string(0).map_err(err)?;
    let revision = rows[1]
        .rows
        .first()
        .map(|r| r.unsigned(0))
        .transpose()
        .map_err(err)?
        .unwrap_or(0);
    let mut users = BTreeMap::new();
    for row in &rows[2].rows {
        let user = UserRecord {
            id: row.parse(0).map_err(err)?,
            name: row.string(1).map_err(err)?,
            active: row.flag(2).map_err(err)?,
            role: org_role_of(row.text(3).map_err(err)?)?,
        };
        users.insert(user.id.clone(), user);
    }
    let mut projects = BTreeMap::new();
    let mut archived_projects = BTreeSet::new();
    for row in &rows[3].rows {
        if row.flag(3).map_err(err)? {
            archived_projects.insert(row.parse(0).map_err(err)?);
        }
        let project = ProjectRecord {
            id: row.parse(0).map_err(err)?,
            org_id: org_id.clone(),
            slug: row.string(1).map_err(err)?,
            title: row.string(2).map_err(err)?,
        };
        projects.insert(project.id.clone(), project);
    }
    let mut teams = BTreeMap::new();
    for row in &rows[4].rows {
        let team = TeamRecord {
            id: row.parse(0).map_err(err)?,
            org_id: org_id.clone(),
            slug: row.string(1).map_err(err)?,
            title: row.string(2).map_err(err)?,
        };
        teams.insert(team.id.clone(), team);
    }
    let mut project_members = BTreeMap::new();
    for row in &rows[5].rows {
        project_members.insert(
            (row.parse(0).map_err(err)?, row.parse(1).map_err(err)?),
            role_of(row.text(2).map_err(err)?)?,
        );
    }
    let mut team_members = BTreeSet::new();
    for row in &rows[6].rows {
        team_members.insert((row.parse(0).map_err(err)?, row.parse(1).map_err(err)?));
    }
    Ok(Some(DirectorySnapshot {
        org_id: org_id.clone(),
        revision,
        organization: OrganizationRecord {
            id: org_id.clone(),
            title,
        },
        users,
        projects,
        teams,
        project_members,
        team_members,
        archived_projects,
    }))
}

/// A planned directory commit: its guarded statements and the revision it
/// commits, or a conflict found while planning.
pub(crate) enum Planned {
    Conflict,
    Write {
        statements: Vec<Statement>,
        revision: u64,
    },
}

/// The global facts a commit's checks need beyond the organization.
#[derive(Default)]
struct Globals {
    identity_names: BTreeMap<String, String>,
    project_ids: BTreeSet<String>,
    team_ids: BTreeSet<String>,
    email_owners: BTreeMap<String, String>,
}

fn has_active_owner(snapshot: &DirectorySnapshot) -> bool {
    snapshot
        .users
        .values()
        .any(|u| u.active && u.role == OrgRole::Owner)
}

impl<D: D1> D1Directory<D> {
    /// The organization's snapshot plus the global facts `mutations` touch,
    /// in one consistent read.
    async fn read_for(
        &self,
        org_id: &OrganizationId,
        mutations: &[DirectoryMutation],
    ) -> Result<(Option<DirectorySnapshot>, Globals), DirectoryError> {
        let mut users = vec![];
        let mut projects = vec![];
        let mut teams = vec![];
        let mut emails = vec![];
        for m in mutations {
            match m {
                DirectoryMutation::CreateUser(u) => users.push(u.id.as_str().to_owned()),
                DirectoryMutation::CreateProject(p) => projects.push(p.id.as_str().to_owned()),
                DirectoryMutation::CreateTeam(t) => teams.push(t.id.as_str().to_owned()),
                DirectoryMutation::BindVerifiedEmail { email, .. } => {
                    if let Ok(key) = email_key(email) {
                        emails.push(key);
                    }
                }
                _ => {}
            }
        }
        let mut reads = snapshot_reads(org_id.as_str());
        reads.push(
            prepare(format!(
                "SELECT user_id, name FROM user_identity WHERE user_id IN ({})",
                placeholders(users.len())
            ))
            .binds(users),
        );
        reads.push(
            prepare(format!(
                "SELECT project_id FROM project WHERE project_id IN ({})",
                placeholders(projects.len())
            ))
            .binds(projects),
        );
        reads.push(
            prepare(format!(
                "SELECT team_id FROM team WHERE team_id IN ({})",
                placeholders(teams.len())
            ))
            .binds(teams),
        );
        reads.push(
            prepare(format!(
                "SELECT email, user_id FROM verified_email WHERE email IN ({})",
                placeholders(emails.len())
            ))
            .binds(emails),
        );
        let rows = self.db.batch(reads).await.map_err(err)?;
        let snapshot = decode_snapshot(org_id, &rows[..SNAPSHOT_READS])?;
        let mut globals = Globals::default();
        for row in &rows[SNAPSHOT_READS].rows {
            globals
                .identity_names
                .insert(row.string(0).map_err(err)?, row.string(1).map_err(err)?);
        }
        for row in &rows[SNAPSHOT_READS + 1].rows {
            globals.project_ids.insert(row.string(0).map_err(err)?);
        }
        for row in &rows[SNAPSHOT_READS + 2].rows {
            globals.team_ids.insert(row.string(0).map_err(err)?);
        }
        for row in &rows[SNAPSHOT_READS + 3].rows {
            globals
                .email_owners
                .insert(row.string(0).map_err(err)?, row.string(1).map_err(err)?);
        }
        Ok((snapshot, globals))
    }

    /// Plan `commit` with the memory directory's semantics. The statements
    /// start with the revision guard and end with the revision update.
    pub(crate) async fn plan(&self, commit: &DirectoryCommit) -> Result<Planned, DirectoryError> {
        let (snapshot, globals) = self.read_for(&commit.org_id, &commit.mutations).await?;
        plan_against(commit, snapshot, &globals)
    }
}

/// Check `commit` against a working copy of `snapshot`; error variants and
/// order match `directory::memory`.
fn plan_against(
    commit: &DirectoryCommit,
    snapshot: Option<DirectorySnapshot>,
    globals: &Globals,
) -> Result<Planned, DirectoryError> {
    let org = commit.org_id.as_str();
    let old = snapshot.as_ref().map_or(0, |s| s.revision);
    if old != commit.expected_revision {
        return Ok(Planned::Conflict);
    }
    if commit.mutations.is_empty() {
        return Err(DirectoryError::Invalid("empty commit"));
    }
    let revision = old.checked_add(1).ok_or(DirectoryError::RevisionOverflow)?;
    let had_owner = snapshot.as_ref().is_some_and(has_active_owner);
    let mut statements = vec![match &snapshot {
        Some(_) => guard(
            "conflict",
            "EXISTS (SELECT 1 FROM directory_revision WHERE org_id = ? AND revision = ?)",
        )
        .bind(org)
        .bind(uint(old).map_err(|_| DirectoryError::RevisionOverflow)?),
        None => guard(
            "conflict",
            "NOT EXISTS (SELECT 1 FROM organization WHERE org_id = ?)",
        )
        .bind(org),
    }];
    let mut working = snapshot;
    let mut identity_names = globals.identity_names.clone();
    let mut project_ids = globals.project_ids.clone();
    let mut team_ids = globals.team_ids.clone();
    let mut email_owners = globals.email_owners.clone();
    for mutation in &commit.mutations {
        if let DirectoryMutation::CreateOrganization { title } = mutation {
            if working.is_some() {
                return Err(DirectoryError::Duplicate);
            }
            if title.trim().is_empty() {
                return Err(DirectoryError::Invalid("title"));
            }
            statements.push(
                prepare("INSERT INTO organization (org_id, title) VALUES (?, ?)")
                    .bind(org)
                    .bind(title),
            );
            statements.push(
                prepare("INSERT INTO directory_revision (org_id, revision) VALUES (?, 0)")
                    .bind(org),
            );
            working = Some(DirectorySnapshot {
                org_id: commit.org_id.clone(),
                revision: 0,
                organization: OrganizationRecord {
                    id: commit.org_id.clone(),
                    title: title.clone(),
                },
                users: BTreeMap::new(),
                projects: BTreeMap::new(),
                teams: BTreeMap::new(),
                project_members: BTreeMap::new(),
                team_members: BTreeSet::new(),
                archived_projects: Default::default(),
            });
            continue;
        }
        let s = working.as_mut().ok_or(DirectoryError::NotFound)?;
        match mutation {
            DirectoryMutation::CreateOrganization { .. } => unreachable!(),
            DirectoryMutation::CreateUser(u) => {
                if u.name.trim().is_empty() {
                    return Err(DirectoryError::Invalid("user name"));
                }
                if s.users.contains_key(&u.id) {
                    return Err(DirectoryError::Duplicate);
                }
                if identity_names
                    .get(u.id.as_str())
                    .is_some_and(|name| name != &u.name)
                {
                    return Err(DirectoryError::Duplicate);
                }
                identity_names.insert(u.id.as_str().to_owned(), u.name.clone());
                statements.push(
                    prepare(
                        "INSERT INTO user_identity (user_id, name) VALUES (?, ?) \
                         ON CONFLICT (user_id) DO NOTHING",
                    )
                    .bind(u.id.as_str())
                    .bind(&u.name),
                );
                statements.push(
                    guard(
                        "duplicate",
                        "EXISTS (SELECT 1 FROM user_identity WHERE user_id = ? AND name = ?)",
                    )
                    .bind(u.id.as_str())
                    .bind(&u.name),
                );
                statements.push(
                    prepare(
                        "INSERT INTO org_user (org_id, user_id, name, active, org_role) \
                         VALUES (?, ?, ?, ?, ?)",
                    )
                    .bind(org)
                    .bind(u.id.as_str())
                    .bind(&u.name)
                    .bind(u.active)
                    .bind(org_role_label(u.role)),
                );
                s.users.insert(u.id.clone(), u.clone());
            }
            DirectoryMutation::CreateProject(p) => {
                if p.org_id != commit.org_id {
                    return Err(DirectoryError::Invalid("organization mismatch"));
                }
                if !slug(&p.slug) || p.title.trim().is_empty() {
                    return Err(DirectoryError::Invalid("project"));
                }
                if project_ids.contains(p.id.as_str())
                    || s.projects.values().any(|x| x.slug == p.slug)
                {
                    return Err(DirectoryError::Duplicate);
                }
                project_ids.insert(p.id.as_str().to_owned());
                statements.push(
                    prepare(
                        "INSERT INTO project (project_id, org_id, slug, title) VALUES (?, ?, ?, ?)",
                    )
                    .bind(p.id.as_str())
                    .bind(org)
                    .bind(&p.slug)
                    .bind(&p.title),
                );
                s.projects.insert(p.id.clone(), p.clone());
            }
            DirectoryMutation::CreateTeam(t) => {
                if t.org_id != commit.org_id {
                    return Err(DirectoryError::Invalid("organization mismatch"));
                }
                if !slug(&t.slug) || t.title.trim().is_empty() {
                    return Err(DirectoryError::Invalid("team"));
                }
                if team_ids.contains(t.id.as_str()) || s.teams.values().any(|x| x.slug == t.slug) {
                    return Err(DirectoryError::Duplicate);
                }
                team_ids.insert(t.id.as_str().to_owned());
                statements.push(
                    prepare("INSERT INTO team (team_id, org_id, slug, title) VALUES (?, ?, ?, ?)")
                        .bind(t.id.as_str())
                        .bind(org)
                        .bind(&t.slug)
                        .bind(&t.title),
                );
                s.teams.insert(t.id.clone(), t.clone());
            }
            DirectoryMutation::SetOrgRole { user_id, role } => {
                let user = s.users.get_mut(user_id).ok_or(DirectoryError::NotFound)?;
                user.role = *role;
                statements.push(
                    prepare("UPDATE org_user SET org_role = ? WHERE org_id = ? AND user_id = ?")
                        .bind(org_role_label(*role))
                        .bind(org)
                        .bind(user_id.as_str()),
                );
            }
            DirectoryMutation::SetProjectArchived {
                project_id,
                archived,
            } => {
                if !s.projects.contains_key(project_id) {
                    return Err(DirectoryError::NotFound);
                }
                if *archived {
                    s.archived_projects.insert(project_id.clone());
                } else {
                    s.archived_projects.remove(project_id);
                }
                statements.push(
                    prepare("UPDATE project SET archived = ? WHERE org_id = ? AND project_id = ?")
                        .bind(*archived)
                        .bind(org)
                        .bind(project_id.as_str()),
                );
            }
            DirectoryMutation::SetProjectTitle { project_id, title } => {
                if title.trim().is_empty() {
                    return Err(DirectoryError::Invalid("project"));
                }
                let project = s
                    .projects
                    .get_mut(project_id)
                    .ok_or(DirectoryError::NotFound)?;
                project.title = title.clone();
                statements.push(
                    prepare("UPDATE project SET title = ? WHERE org_id = ? AND project_id = ?")
                        .bind(title.as_str())
                        .bind(org)
                        .bind(project_id.as_str()),
                );
            }
            DirectoryMutation::SetUserName { user_id, name } => {
                if name.trim().is_empty() {
                    return Err(DirectoryError::Invalid("user name"));
                }
                let user = s.users.get_mut(user_id).ok_or(DirectoryError::NotFound)?;
                user.name = name.clone();
                // One identity: its name and every membership's copy of it.
                for sql in [
                    "UPDATE user_identity SET name = ? WHERE user_id = ?",
                    "UPDATE org_user SET name = ? WHERE user_id = ?",
                ] {
                    statements.push(prepare(sql).bind(name.as_str()).bind(user_id.as_str()));
                }
            }
            DirectoryMutation::SetUserActive { user_id, active } => {
                let user = s.users.get_mut(user_id).ok_or(DirectoryError::NotFound)?;
                user.active = *active;
                statements.push(
                    prepare("UPDATE org_user SET active = ? WHERE org_id = ? AND user_id = ?")
                        .bind(*active)
                        .bind(org)
                        .bind(user_id.as_str()),
                );
            }
            DirectoryMutation::GrantProjectRole {
                project_id,
                user_id,
                role,
            } => {
                if !s.projects.contains_key(project_id) || !s.users.contains_key(user_id) {
                    return Err(DirectoryError::NotFound);
                }
                s.project_members
                    .insert((project_id.clone(), user_id.clone()), *role);
                statements.push(
                    prepare(
                        "INSERT INTO project_member (org_id, project_id, user_id, role) \
                         VALUES (?, ?, ?, ?) ON CONFLICT (project_id, user_id) \
                         DO UPDATE SET role = excluded.role",
                    )
                    .bind(org)
                    .bind(project_id.as_str())
                    .bind(user_id.as_str())
                    .bind(role_label(*role)),
                );
            }
            DirectoryMutation::RevokeProjectRole {
                project_id,
                user_id,
            } => {
                if !s.projects.contains_key(project_id) || !s.users.contains_key(user_id) {
                    return Err(DirectoryError::NotFound);
                }
                s.project_members
                    .remove(&(project_id.clone(), user_id.clone()));
                statements.push(
                    prepare(
                        "DELETE FROM project_member WHERE org_id = ? AND project_id = ? AND user_id = ?",
                    )
                    .bind(org)
                    .bind(project_id.as_str())
                    .bind(user_id.as_str()),
                );
            }
            DirectoryMutation::AddTeamMember { team_id, user_id } => {
                if !s.teams.contains_key(team_id) || !s.users.contains_key(user_id) {
                    return Err(DirectoryError::NotFound);
                }
                s.team_members.insert((team_id.clone(), user_id.clone()));
                statements.push(
                    prepare(
                        "INSERT INTO team_member (org_id, team_id, user_id) VALUES (?, ?, ?) \
                         ON CONFLICT (team_id, user_id) DO NOTHING",
                    )
                    .bind(org)
                    .bind(team_id.as_str())
                    .bind(user_id.as_str()),
                );
            }
            DirectoryMutation::RemoveTeamMember { team_id, user_id } => {
                if !s.teams.contains_key(team_id) || !s.users.contains_key(user_id) {
                    return Err(DirectoryError::NotFound);
                }
                s.team_members.remove(&(team_id.clone(), user_id.clone()));
                statements.push(
                    prepare(
                        "DELETE FROM team_member WHERE org_id = ? AND team_id = ? AND user_id = ?",
                    )
                    .bind(org)
                    .bind(team_id.as_str())
                    .bind(user_id.as_str()),
                );
            }
            DirectoryMutation::BindVerifiedEmail { user_id, email } => {
                if !s.users.contains_key(user_id) {
                    return Err(DirectoryError::NotFound);
                }
                let key = email_key(email)?;
                if email_owners
                    .get(&key)
                    .is_some_and(|owner| owner != user_id.as_str())
                {
                    return Err(DirectoryError::Duplicate);
                }
                email_owners.insert(key.clone(), user_id.as_str().to_owned());
                statements.push(
                    prepare(
                        "INSERT INTO verified_email (email, user_id) VALUES (?, ?) \
                         ON CONFLICT (email) DO NOTHING",
                    )
                    .bind(&key)
                    .bind(user_id.as_str()),
                );
                statements.push(
                    guard(
                        "duplicate",
                        "EXISTS (SELECT 1 FROM verified_email WHERE email = ? AND user_id = ?)",
                    )
                    .bind(&key)
                    .bind(user_id.as_str()),
                );
            }
        }
    }
    if had_owner && !working.as_ref().is_some_and(has_active_owner) {
        return Err(DirectoryError::Invalid("last owner"));
    }
    statements.push(
        prepare("UPDATE directory_revision SET revision = ? WHERE org_id = ?")
            .bind(uint(revision).map_err(|_| DirectoryError::RevisionOverflow)?)
            .bind(org),
    );
    Ok(Planned::Write {
        statements,
        revision,
    })
}

/// The revision bump alone (an operation that only writes variables).
fn bump(org: &str, old: u64) -> Result<(Vec<Statement>, u64), DirectoryError> {
    let next = old.checked_add(1).ok_or(DirectoryError::RevisionOverflow)?;
    let old = uint(old).map_err(|_| DirectoryError::RevisionOverflow)?;
    let next_value = uint(next).map_err(|_| DirectoryError::RevisionOverflow)?;
    Ok((
        vec![
            guard(
                "conflict",
                "EXISTS (SELECT 1 FROM directory_revision WHERE org_id = ? AND revision = ?)",
            )
            .bind(org)
            .bind(old),
            prepare("UPDATE directory_revision SET revision = ? WHERE org_id = ?")
                .bind(next_value)
                .bind(org),
        ],
        next,
    ))
}

/// A batch that may end in the planned guard's conflict.
async fn write<D: D1>(db: &D, statements: Vec<Statement>) -> Result<bool, DirectoryError> {
    match db.batch(statements).await {
        Ok(_) => Ok(true),
        Err(D1Error::Guard(code)) if code == "conflict" => Ok(false),
        Err(error) => Err(err(error)),
    }
}

impl<D: D1> runbook_core::pictures::PictureStore for D1Directory<D> {
    async fn picture(
        &self,
        owner: &runbook_core::pictures::PictureOwner,
    ) -> Result<Option<runbook_core::pictures::Picture>, DirectoryError> {
        use runbook_core::pictures::{Picture, PictureOwner};
        let statement = match owner {
            PictureOwner::User(user) => prepare(
                "SELECT content_type, bytes, source_url, updated_at \
                 FROM user_picture WHERE user_id = ?",
            )
            .bind(user.as_str()),
            PictureOwner::Project(org, project) => prepare(
                "SELECT content_type, bytes, NULL, updated_at \
                 FROM project_picture WHERE org_id = ? AND project_id = ?",
            )
            .bind(org.as_str())
            .bind(project.as_str()),
        };
        self.db
            .all(statement)
            .await
            .map_err(err)?
            .first()
            .map(|row| {
                Ok(Picture {
                    content_type: row.string(0).map_err(err)?,
                    bytes: row.blob(1).map_err(err)?.to_vec(),
                    source_url: row.optional_text(2).map_err(err)?,
                    updated_at_ms: row.integer(3).map_err(err)?,
                })
            })
            .transpose()
    }
    async fn set_picture(
        &self,
        owner: &runbook_core::pictures::PictureOwner,
        picture: Option<runbook_core::pictures::Picture>,
    ) -> Result<(), DirectoryError> {
        use runbook_core::pictures::PictureOwner;
        let statement = match (owner, &picture) {
            (PictureOwner::User(user), Some(p)) => prepare(
                "INSERT INTO user_picture (user_id, content_type, bytes, source_url, updated_at) \
                 VALUES (?, ?, ?, ?, ?) ON CONFLICT (user_id) DO UPDATE SET \
                 content_type = excluded.content_type, bytes = excluded.bytes, \
                 source_url = excluded.source_url, updated_at = excluded.updated_at",
            )
            .bind(user.as_str())
            .bind(p.content_type.as_str())
            .bind(&p.bytes[..])
            .bind(p.source_url.as_deref())
            .bind(p.updated_at_ms),
            (PictureOwner::User(user), None) => {
                prepare("DELETE FROM user_picture WHERE user_id = ?").bind(user.as_str())
            }
            (PictureOwner::Project(org, project), Some(p)) => prepare(
                "INSERT INTO project_picture (org_id, project_id, content_type, bytes, updated_at) \
                 VALUES (?, ?, ?, ?, ?) ON CONFLICT (org_id, project_id) DO UPDATE SET \
                 content_type = excluded.content_type, bytes = excluded.bytes, \
                 updated_at = excluded.updated_at",
            )
            .bind(org.as_str())
            .bind(project.as_str())
            .bind(p.content_type.as_str())
            .bind(&p.bytes[..])
            .bind(p.updated_at_ms),
            (PictureOwner::Project(org, project), None) => {
                prepare("DELETE FROM project_picture WHERE org_id = ? AND project_id = ?")
                    .bind(org.as_str())
                    .bind(project.as_str())
            }
        };
        self.db.run(statement).await.map_err(err)?;
        Ok(())
    }
}
impl<D: D1> DirectoryStore for D1Directory<D> {
    async fn snapshot(&self, org_id: &OrganizationId) -> Result<DirectorySnapshot, DirectoryError> {
        let rows = self
            .db
            .batch(snapshot_reads(org_id.as_str()))
            .await
            .map_err(err)?;
        decode_snapshot(org_id, &rows)?.ok_or(DirectoryError::NotFound)
    }

    async fn commit(&self, commit: DirectoryCommit) -> Result<CommitResult, DirectoryError> {
        match self.plan(&commit).await? {
            Planned::Conflict => Ok(CommitResult::Conflict),
            Planned::Write {
                statements,
                revision,
            } => Ok(if write(&self.db, statements).await? {
                CommitResult::Committed(revision)
            } else {
                CommitResult::Conflict
            }),
        }
    }

    async fn user_by_id(&self, user_id: &UserId) -> Result<Option<UserIdentity>, DirectoryError> {
        self.db
            .all(prepare("SELECT name FROM user_identity WHERE user_id = ?").bind(user_id.as_str()))
            .await
            .map_err(err)?
            .first()
            .map(|row| {
                Ok(UserIdentity {
                    id: user_id.clone(),
                    name: row.string(0).map_err(err)?,
                })
            })
            .transpose()
    }

    async fn user_by_verified_email(
        &self,
        email: &str,
    ) -> Result<Option<UserIdentity>, DirectoryError> {
        let key = email_key(email)?;
        self.db
            .all(
                prepare(
                    "SELECT i.user_id, i.name FROM verified_email e \
                     JOIN user_identity i ON i.user_id = e.user_id WHERE e.email = ?",
                )
                .bind(key),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| {
                Ok(UserIdentity {
                    id: row.parse(0).map_err(err)?,
                    name: row.string(1).map_err(err)?,
                })
            })
            .transpose()
    }

    async fn verified_emails(
        &self,
        org_id: &OrganizationId,
    ) -> Result<std::collections::BTreeMap<UserId, String>, DirectoryError> {
        // Snapshot first: an unknown org is NotFound, not an empty map.
        self.snapshot(org_id).await?;
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT e.user_id, e.email FROM verified_email e \
                     JOIN org_user ou ON ou.user_id = e.user_id WHERE ou.org_id = ?",
                )
                .bind(org_id.as_str()),
            )
            .await
            .map_err(err)?;
        rows.iter()
            .map(|row| Ok((row.parse(0).map_err(err)?, row.string(1).map_err(err)?)))
            .collect()
    }

    async fn org_for_project(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<OrganizationId>, DirectoryError> {
        self.db
            .all(
                prepare("SELECT org_id FROM project WHERE project_id = ?")
                    .bind(project_id.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| row.parse(0).map_err(err))
            .transpose()
    }
}

/// One day: how long a directory POST replay is kept (as on Postgres).
const REPLAY_TTL_MS: i64 = 86_400_000;

fn replay_read(org: &str, r: &RequestReplay, now: i64) -> Statement {
    prepare(
        "SELECT payload, response FROM directory_request WHERE org_id = ? AND principal = ? \
         AND operation = ? AND key = ? AND expires_at > ?",
    )
    .bind(org)
    .bind(&r.principal)
    .bind(&r.operation)
    .bind(&r.key)
    .bind(now)
}
async fn replay_of(
    rows: &[crate::sql::Row],
    r: &RequestReplay,
    org: &OrganizationId,
    blobs: Option<&dyn crate::replay::ReplayBlobs>,
) -> Result<ReplayResult, DirectoryError> {
    let Some(row) = rows.first() else {
        return Ok(ReplayResult::Missing);
    };
    if !crate::replay::matches(row.text(0).map_err(err)?, &r.payload) {
        return Ok(ReplayResult::KeyReused);
    }
    crate::replay::read_reply(row.text(1).map_err(err)?, org, blobs)
        .await
        .map(ReplayResult::Applied)
        .map_err(|_| DirectoryError::Backend("d1.replay_response"))
}

impl<D: D1> DirectoryOperations for D1Directory<D> {
    async fn lookup_replay(
        &self,
        org: &OrganizationId,
        expected_revision: u64,
        replay: &RequestReplay,
    ) -> Result<ReplayResult, DirectoryError> {
        let now = self.db.now_ms();
        let rows = self
            .db
            .batch(vec![
                prepare("SELECT revision FROM directory_revision WHERE org_id = ?")
                    .bind(org.as_str()),
                replay_read(org.as_str(), replay, now),
            ])
            .await
            .map_err(err)?;
        let revision = rows[0]
            .rows
            .first()
            .ok_or(DirectoryError::NotFound)?
            .unsigned(0)
            .map_err(err)?;
        if revision != expected_revision {
            return Ok(ReplayResult::Conflict);
        }
        replay_of(&rows[1].rows, replay, org, self.replay_blobs.as_deref()).await
    }

    async fn current_time_ms(&self) -> Result<i64, DirectoryError> {
        Ok(self.db.now_ms())
    }

    async fn project_variables(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
    ) -> Result<VariablesRecord, DirectoryError> {
        let rows = self
            .db
            .batch(vec![
                prepare("SELECT 1 FROM project WHERE project_id = ? AND org_id = ?")
                    .bind(project.as_str())
                    .bind(org.as_str()),
                prepare(
                    "SELECT body, revision FROM project_variables WHERE org_id = ? AND project_id = ?",
                )
                .bind(org.as_str())
                .bind(project.as_str()),
            ])
            .await
            .map_err(err)?;
        if rows[0].rows.is_empty() {
            return Err(DirectoryError::NotFound);
        }
        rows[1]
            .rows
            .first()
            .map(|r| {
                Ok(VariablesRecord {
                    variables: r.json(0).map_err(err)?,
                    revision: r.unsigned(1).map_err(err)?,
                })
            })
            .transpose()
            .map(Option::unwrap_or_default)
    }

    async fn apply_operation(
        &self,
        op: DirectoryOperation,
    ) -> Result<OperationResult, DirectoryError> {
        let org = op.commit.org_id.as_str();
        let now = self.db.now_ms();
        // The replay row and the variables, read with the plan's snapshot
        // revision; the batch guards both.
        let mut reads =
            vec![prepare("SELECT revision FROM directory_revision WHERE org_id = ?").bind(org)];
        if let Some(r) = &op.replay {
            reads.push(replay_read(org, r, now));
        }
        if let Some(v) = &op.variables {
            reads.push(
                prepare("SELECT 1 FROM project WHERE project_id = ? AND org_id = ?")
                    .bind(v.project_id.as_str())
                    .bind(org),
            );
            reads.push(
                prepare(
                    "SELECT revision FROM project_variables WHERE org_id = ? AND project_id = ?",
                )
                .bind(org)
                .bind(v.project_id.as_str()),
            );
        }
        let rows = self.db.batch(reads).await.map_err(err)?;
        let old = rows[0]
            .rows
            .first()
            .ok_or(DirectoryError::NotFound)?
            .unsigned(0)
            .map_err(err)?;
        if old != op.commit.expected_revision {
            return Ok(OperationResult::Conflict);
        }
        let mut next = 1;
        if let Some(r) = &op.replay {
            match replay_of(
                &rows[next].rows,
                r,
                &op.commit.org_id,
                self.replay_blobs.as_deref(),
            )
            .await?
            {
                ReplayResult::Applied(response) => return Ok(OperationResult::Applied(response)),
                ReplayResult::KeyReused => return Ok(OperationResult::KeyReused),
                ReplayResult::Missing => {}
                ReplayResult::Conflict => return Ok(OperationResult::Conflict),
            }
            next += 1;
        }
        let mut statements = if op.commit.mutations.is_empty() {
            if op.variables.is_none() {
                return Err(DirectoryError::Invalid("empty operation"));
            }
            bump(org, old)?.0
        } else {
            match self.plan(&op.commit).await? {
                Planned::Conflict => return Ok(OperationResult::Conflict),
                Planned::Write { statements, .. } => statements,
            }
        };
        if let Some(v) = &op.variables {
            if rows[next].rows.is_empty() {
                return Err(DirectoryError::NotFound);
            }
            let stored = rows[next + 1]
                .rows
                .first()
                .map(|r| r.unsigned(0))
                .transpose()
                .map_err(err)?
                .unwrap_or(0);
            if stored != v.expected_revision {
                return Ok(OperationResult::VariablesConflict);
            }
            let revision = stored
                .checked_add(1)
                .ok_or(DirectoryError::RevisionOverflow)?;
            // The variables row is fenced by the directory revision guard:
            // every variables write bumps the revision too.
            statements.push(
                prepare(
                    "INSERT INTO project_variables (org_id, project_id, body, revision) \
                     VALUES (?, ?, ?, ?) ON CONFLICT (project_id) DO UPDATE SET \
                     body = excluded.body, revision = excluded.revision \
                     WHERE project_variables.org_id = excluded.org_id",
                )
                .bind(org)
                .bind(v.project_id.as_str())
                .bind(json(&v.variables).map_err(err)?)
                .bind(uint(revision).map_err(|_| DirectoryError::RevisionOverflow)?),
            );
        }
        if let Some(r) = &op.replay {
            // CF12b: a row whose response is in R2 goes with its object
            // (`crate::replay::sweep`), not here.
            statements.push(
                prepare(format!(
                    "DELETE FROM directory_request WHERE org_id = ? AND expires_at <= ? AND {}",
                    crate::replay::SWEEPABLE
                ))
                .bind(org)
                .bind(now),
            );
            statements.push(
                prepare(
                    "INSERT INTO directory_request (org_id, principal, operation, key, payload, \
                     response, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(org)
                .bind(&r.principal)
                .bind(&r.operation)
                .bind(&r.key)
                .bind(crate::replay::stored(&r.payload))
                .bind(json(&op.response).map_err(err)?)
                .bind(now.saturating_add(REPLAY_TTL_MS)),
            );
        }
        Ok(if write(&self.db, statements).await? {
            OperationResult::Applied(op.response)
        } else {
            OperationResult::Conflict
        })
    }
}

fn state_of(text: &str) -> Result<InvitationState, DirectoryError> {
    match text {
        "pending" => Ok(InvitationState::Pending),
        "accepted" => Ok(InvitationState::Accepted),
        "revoked" => Ok(InvitationState::Revoked),
        _ => Err(DirectoryError::Backend("d1.invalid_invitation")),
    }
}
const INVITATION_COLUMNS: &str = "invitation_id, org_id, email, role, projects, invited_by, \
     created_at, expires_at, state, accepted_by, closed_at, name";
fn invitation(row: &crate::sql::Row) -> Result<InvitationRecord, DirectoryError> {
    Ok(InvitationRecord {
        id: row.parse(0).map_err(err)?,
        org_id: row.parse(1).map_err(err)?,
        email: row.string(2).map_err(err)?,
        role: org_role_of(row.text(3).map_err(err)?)?,
        projects: row.json(4).map_err(err)?,
        invited_by: row.parse(5).map_err(err)?,
        created_at_ms: row.integer(6).map_err(err)?,
        expires_at_ms: row.integer(7).map_err(err)?,
        state: state_of(row.text(8).map_err(err)?)?,
        accepted_by: row.optional_parse(9).map_err(err)?,
        closed_at_ms: row.optional_integer(10).map_err(err)?,
        name: row.optional_text(11).map_err(err)?,
    })
}
const LINK_COLUMNS: &str = "auth_user_id, org_id, user_id, email, linked_at";
fn link(row: &crate::sql::Row) -> Result<AuthLink, DirectoryError> {
    Ok(AuthLink {
        auth_user_id: row.string(0).map_err(err)?,
        org_id: row.parse(1).map_err(err)?,
        user_id: row.parse(2).map_err(err)?,
        email: row.string(3).map_err(err)?,
        linked_at_ms: row.integer(4).map_err(err)?,
    })
}

/// How many times a link is re-planned when a racing directory commit moved
/// the revision between its read and its write.
const LINK_ATTEMPTS: usize = 3;

impl<D: D1> D1Directory<D> {
    /// Organizations where the user is an active member, by ID (a global key
    /// read for linking a session with no invitation, D45; grants nothing).
    pub async fn active_orgs_of_user(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<OrganizationId>, DirectoryError> {
        self.db
            .all(
                prepare("SELECT org_id FROM org_user WHERE user_id = ? AND active ORDER BY org_id")
                    .bind(user_id.as_str()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| row.parse(0).map_err(err))
            .collect()
    }

    async fn link_once(
        &self,
        org: &OrganizationId,
        request: &LinkRequest,
    ) -> Result<Option<AuthLink>, DirectoryError> {
        let email = email_key(&request.verified_email)?;
        let mut reads = snapshot_reads(org.as_str());
        reads.push(
            prepare(format!(
                "SELECT {LINK_COLUMNS} FROM auth_link WHERE auth_user_id = ?"
            ))
            .bind(&request.auth_user_id),
        );
        reads.push(
            prepare(format!(
                "SELECT {INVITATION_COLUMNS} FROM invitation WHERE invitation_id = ?"
            ))
            .bind(request.invitation.as_ref().map(|i| i.as_str().to_owned())),
        );
        reads.push(prepare("SELECT user_id FROM verified_email WHERE email = ?").bind(&email));
        reads.push(
            prepare("SELECT name FROM user_identity WHERE user_id = ?")
                .bind(request.user_id.as_str()),
        );
        let rows = self.db.batch(reads).await.map_err(err)?;
        let snapshot =
            decode_snapshot(org, &rows[..SNAPSHOT_READS])?.ok_or(DirectoryError::NotFound)?;
        let existing = rows[SNAPSHOT_READS].rows.first().map(link).transpose()?;
        let named = rows[SNAPSHOT_READS + 1]
            .rows
            .first()
            .map(invitation)
            .transpose()?;
        let owner: Option<UserId> = rows[SNAPSHOT_READS + 2]
            .rows
            .first()
            .map(|r| r.parse(0).map_err(err))
            .transpose()?;
        let identity_name = rows[SNAPSHOT_READS + 3]
            .rows
            .first()
            .map(|r| r.string(0).map_err(err))
            .transpose()?;
        let plan = plan_link(
            org,
            request,
            LinkState {
                existing: existing.as_ref(),
                invitation: named.as_ref(),
                email_owner: owner.as_ref(),
                snapshot: &snapshot,
                identity_name: identity_name.as_deref(),
            },
        )?;
        let (mutations, new_link, accept) = match plan {
            LinkPlan::Existing(link) => return Ok(Some(link)),
            LinkPlan::Commit {
                mutations,
                link,
                accept,
            } => (mutations, link, accept),
        };
        let commit = DirectoryCommit {
            org_id: org.clone(),
            expected_revision: snapshot.revision,
            mutations,
        };
        let mut statements = match self.plan(&commit).await? {
            Planned::Conflict => return Ok(None),
            Planned::Write { statements, .. } => statements,
        };
        if let Some(id) = accept {
            statements.push(
                guard(
                    "invitation",
                    "EXISTS (SELECT 1 FROM invitation WHERE invitation_id = ? AND state = 'pending')",
                )
                .bind(id.as_str()),
            );
            statements.push(
                prepare(
                    "UPDATE invitation SET state = 'accepted', accepted_by = ?, closed_at = ? \
                     WHERE invitation_id = ? AND state = 'pending'",
                )
                .bind(request.user_id.as_str())
                .bind(request.now_ms)
                .bind(id.as_str()),
            );
        }
        statements.push(
            prepare(
                "INSERT INTO auth_link (auth_user_id, org_id, user_id, email, linked_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&new_link.auth_user_id)
            .bind(new_link.org_id.as_str())
            .bind(new_link.user_id.as_str())
            .bind(&new_link.email)
            .bind(new_link.linked_at_ms),
        );
        Ok(write(&self.db, statements).await?.then_some(new_link))
    }
}

impl<D: D1> AuthLinkStore for D1Directory<D> {
    async fn auth_link(&self, auth_user_id: &str) -> Result<Option<AuthLink>, DirectoryError> {
        self.db
            .all(
                prepare(format!(
                    "SELECT {LINK_COLUMNS} FROM auth_link WHERE auth_user_id = ?"
                ))
                .bind(auth_user_id),
            )
            .await
            .map_err(err)?
            .first()
            .map(link)
            .transpose()
    }
    async fn auth_users_of(&self, user_id: &UserId) -> Result<Vec<String>, DirectoryError> {
        self.db
            .all(
                prepare(
                    "SELECT auth_user_id FROM auth_link WHERE user_id = ? ORDER BY auth_user_id",
                )
                .bind(user_id.as_str()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| row.string(0).map_err(err))
            .collect()
    }
    async fn invitations_for_email(
        &self,
        email: &str,
        now_ms: i64,
    ) -> Result<Vec<InvitationRecord>, DirectoryError> {
        let key = email_key(email)?;
        self.db
            .all(
                prepare(format!(
                    "SELECT {INVITATION_COLUMNS} FROM invitation WHERE email = ? \
                     AND state = 'pending' AND expires_at > ? \
                     ORDER BY created_at DESC, invitation_id DESC"
                ))
                .bind(key)
                .bind(now_ms),
            )
            .await
            .map_err(err)?
            .iter()
            .map(invitation)
            .collect()
    }
    async fn link_identity(
        &self,
        org: &OrganizationId,
        request: LinkRequest,
    ) -> Result<AuthLink, DirectoryError> {
        for _ in 0..LINK_ATTEMPTS {
            if let Some(link) = self.link_once(org, &request).await? {
                return Ok(link);
            }
        }
        Err(DirectoryError::Backend("d1.retry"))
    }
    async fn create_invitation(
        &self,
        org: &OrganizationId,
        record: InvitationRecord,
    ) -> Result<(), DirectoryError> {
        let snapshot = self.snapshot(org).await?;
        let record = validate_invitation(org, &record, &snapshot)?;
        let projects = serde_json::to_string(&record.projects)
            .map_err(|_| DirectoryError::Invalid("projects"))?;
        let statements = vec![
            // The projects it grants were checked at this revision.
            guard(
                "conflict",
                "EXISTS (SELECT 1 FROM directory_revision WHERE org_id = ? AND revision = ?)",
            )
            .bind(org.as_str())
            .bind(uint(snapshot.revision).map_err(err)?),
            guard(
                "duplicate",
                "NOT EXISTS (SELECT 1 FROM invitation WHERE org_id = ? AND email = ? \
                 AND state = 'pending' AND expires_at > ?)",
            )
            .bind(org.as_str())
            .bind(&record.email)
            .bind(record.created_at_ms),
            prepare(
                "INSERT INTO invitation (invitation_id, org_id, email, role, projects, \
                 invited_by, created_at, expires_at, state, name) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)",
            )
            .bind(record.id.as_str())
            .bind(org.as_str())
            .bind(&record.email)
            .bind(org_role_label(record.role))
            .bind(projects)
            .bind(record.invited_by.as_str())
            .bind(record.created_at_ms)
            .bind(record.expires_at_ms)
            .bind(record.name.as_deref()),
        ];
        match self.db.batch(statements).await {
            Ok(_) => Ok(()),
            Err(D1Error::Guard(code)) if code == "conflict" => {
                Err(DirectoryError::Backend("d1.retry"))
            }
            Err(error) => Err(err(error)),
        }
    }
    async fn list_invitations(
        &self,
        org: &OrganizationId,
    ) -> Result<Vec<InvitationRecord>, DirectoryError> {
        self.db
            .all(
                prepare(format!(
                    "SELECT {INVITATION_COLUMNS} FROM invitation WHERE org_id = ? \
                     ORDER BY created_at DESC, invitation_id DESC"
                ))
                .bind(org.as_str()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(invitation)
            .collect()
    }
    async fn revoke_invitation(
        &self,
        org: &OrganizationId,
        id: &InvitationId,
        now_ms: i64,
    ) -> Result<bool, DirectoryError> {
        Ok(self
            .db
            .run(
                prepare(
                    "UPDATE invitation SET state = 'revoked', closed_at = ? \
                     WHERE org_id = ? AND invitation_id = ? AND state = 'pending'",
                )
                .bind(now_ms)
                .bind(org.as_str())
                .bind(id.as_str()),
            )
            .await
            .map_err(err)?
            == 1)
    }
}
