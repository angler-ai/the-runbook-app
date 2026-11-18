//! B1 DirectoryStore on PostgreSQL. One READ COMMITTED transaction locks the
//! organization's revision row, compares the expected revision, and applies
//! the mutations with the memory directory's exact semantics, or none.
use crate::{
    DatabaseConfig,
    execution::{READ_COMMITTED, SNAPSHOT_READ},
    pool::{Conn, Pool},
};
use runbook_core::{
    CommitResult,
    directory::{
        DirectoryCommit, DirectoryError, DirectoryMutation, DirectorySnapshot, DirectoryStore,
        OrgRole, OrganizationRecord, ProjectRecord, TeamRecord, UserIdentity, UserRecord,
        email_key, slug,
    },
};
use runbook_model::{OrganizationId, ProjectId, ProjectRole, TeamId, UserId};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use tokio_postgres::{Client, error::SqlState};

/// Directory handle. Clones share one connection pool; futures are not Send.
#[derive(Clone)]
pub struct PostgresDirectory {
    pool: Rc<Pool>,
}
impl PostgresDirectory {
    pub fn new(config: DatabaseConfig) -> Self {
        Self {
            pool: Pool::new(config),
        }
    }
}

/// Driver failures carry the same codes as rows::db inside Backend.
/// A unique violation is a racing duplicate.
fn db(error: tokio_postgres::Error) -> DirectoryError {
    match error.code() {
        Some(code) if *code == SqlState::UNIQUE_VIOLATION => DirectoryError::Duplicate,
        Some(code) if *code == SqlState::LOCK_NOT_AVAILABLE => {
            DirectoryError::Backend("postgres.lock_timeout")
        }
        Some(code)
            if *code == SqlState::T_R_DEADLOCK_DETECTED
                || *code == SqlState::T_R_SERIALIZATION_FAILURE =>
        {
            DirectoryError::Backend("postgres.retry_transaction")
        }
        _ => DirectoryError::Backend("postgres.database_error"),
    }
}
fn connection(_: crate::Error) -> DirectoryError {
    DirectoryError::Backend("postgres.connection_failed")
}
fn parse<T: std::str::FromStr>(text: &str) -> Result<T, DirectoryError> {
    text.parse()
        .map_err(|_| DirectoryError::Backend("postgres.invalid_stored_id"))
}
fn uint(value: i64) -> Result<u64, DirectoryError> {
    u64::try_from(value).map_err(|_| DirectoryError::Backend("postgres.negative_counter"))
}
fn int(value: u64) -> Result<i64, DirectoryError> {
    i64::try_from(value).map_err(|_| DirectoryError::RevisionOverflow)
}
pub(crate) fn org_role_label(role: OrgRole) -> &'static str {
    match role {
        OrgRole::Owner => "owner",
        OrgRole::Admin => "admin",
        OrgRole::Member => "member",
    }
}
fn org_role_of(text: &str) -> Result<OrgRole, DirectoryError> {
    match text {
        "owner" => Ok(OrgRole::Owner),
        "admin" => Ok(OrgRole::Admin),
        "member" => Ok(OrgRole::Member),
        _ => Err(DirectoryError::Backend("postgres.invalid_org_role")),
    }
}
async fn has_active_owner(c: &Client, org: &str) -> Result<bool, DirectoryError> {
    Ok(c.query_one("SELECT EXISTS (SELECT 1 FROM runbook.org_user WHERE org_id = $1 AND active AND org_role = 'owner')", &[&org]).await.map_err(db)?.get(0))
}
pub(crate) fn role_label(role: ProjectRole) -> &'static str {
    match role {
        ProjectRole::Viewer => "viewer",
        ProjectRole::Operator => "operator",
        ProjectRole::Editor => "editor",
        ProjectRole::Admin => "admin",
    }
}
fn role_of(text: &str) -> Result<ProjectRole, DirectoryError> {
    match text {
        "viewer" => Ok(ProjectRole::Viewer),
        "operator" => Ok(ProjectRole::Operator),
        "editor" => Ok(ProjectRole::Editor),
        "admin" => Ok(ProjectRole::Admin),
        _ => Err(DirectoryError::Backend("postgres.invalid_role")),
    }
}

/// Commit on success; roll back and keep the original error otherwise.
async fn finish<T>(
    conn: &mut Conn,
    result: Result<T, DirectoryError>,
) -> Result<T, DirectoryError> {
    match result {
        Ok(value) => {
            conn.commit().await.map_err(db)?;
            Ok(value)
        }
        Err(error) => {
            conn.rollback().await;
            Err(error)
        }
    }
}
/// A conflict rolls back too, so no part of a losing batch survives.
async fn finish_commit(
    conn: &mut Conn,
    result: Result<CommitResult, DirectoryError>,
) -> Result<CommitResult, DirectoryError> {
    match result {
        Ok(CommitResult::Conflict) => {
            conn.rollback().await;
            Ok(CommitResult::Conflict)
        }
        other => finish(conn, other).await,
    }
}

async fn org_user_exists(c: &Client, org: &str, user: &UserId) -> Result<bool, DirectoryError> {
    c.query_one(
        "SELECT EXISTS (SELECT 1 FROM runbook.org_user WHERE org_id = $1 AND user_id = $2)",
        &[&org, &user.as_str()],
    )
    .await
    .map(|row| row.get(0))
    .map_err(db)
}
async fn project_in_org(
    c: &Client,
    org: &str,
    project: &ProjectId,
) -> Result<bool, DirectoryError> {
    c.query_one(
        "SELECT EXISTS (SELECT 1 FROM runbook.project WHERE project_id = $1 AND org_id = $2)",
        &[&project.as_str(), &org],
    )
    .await
    .map(|row| row.get(0))
    .map_err(db)
}
async fn team_in_org(c: &Client, org: &str, team: &TeamId) -> Result<bool, DirectoryError> {
    c.query_one(
        "SELECT EXISTS (SELECT 1 FROM runbook.team WHERE team_id = $1 AND org_id = $2)",
        &[&team.as_str(), &org],
    )
    .await
    .map(|row| row.get(0))
    .map_err(db)
}

/// Checks then writes, inside the caller's transaction with the revision row
/// locked. Mutation order and error variants match directory::memory::apply.
pub(crate) async fn apply(
    c: &Client,
    commit: DirectoryCommit,
) -> Result<CommitResult, DirectoryError> {
    let org = commit.org_id.as_str();
    let locked = c
        .query_opt(
            "SELECT revision FROM runbook.directory_revision WHERE org_id = $1 FOR UPDATE",
            &[&org],
        )
        .await
        .map_err(db)?;
    let old = match &locked {
        Some(row) => uint(row.get(0))?,
        None => 0,
    };
    if old != commit.expected_revision {
        return Ok(CommitResult::Conflict);
    }
    if commit.mutations.is_empty() {
        return Err(DirectoryError::Invalid("empty commit"));
    }
    let revision = old.checked_add(1).ok_or(DirectoryError::RevisionOverflow)?;
    let had_owner = has_active_owner(c, org).await?;
    // The revision row is inserted with the organization, so it marks existence.
    let mut exists = locked.is_some();
    for mutation in commit.mutations {
        if let DirectoryMutation::CreateOrganization { title } = &mutation {
            if exists {
                return Err(DirectoryError::Duplicate);
            }
            if title.trim().is_empty() {
                return Err(DirectoryError::Invalid("title"));
            }
            // Two creators race on the primary key; the loser gets Conflict.
            let inserted = c
                .execute(
                    "INSERT INTO runbook.organization (org_id, title) VALUES ($1, $2) \
                     ON CONFLICT (org_id) DO NOTHING",
                    &[&org, title],
                )
                .await
                .map_err(db)?;
            if inserted == 0 {
                return Ok(CommitResult::Conflict);
            }
            c.execute(
                "INSERT INTO runbook.directory_revision (org_id, revision) VALUES ($1, 0)",
                &[&org],
            )
            .await
            .map_err(db)?;
            exists = true;
            continue;
        }
        if !exists {
            return Err(DirectoryError::NotFound);
        }
        match mutation {
            DirectoryMutation::CreateOrganization { .. } => unreachable!(),
            DirectoryMutation::CreateUser(u) => {
                if u.name.trim().is_empty() {
                    return Err(DirectoryError::Invalid("user name"));
                }
                if org_user_exists(c, org, &u.id).await? {
                    return Err(DirectoryError::Duplicate);
                }
                c.execute(
                    "INSERT INTO runbook.user_identity (user_id, name) VALUES ($1, $2) \
                     ON CONFLICT (user_id) DO NOTHING",
                    &[&u.id.as_str(), &u.name],
                )
                .await
                .map_err(db)?;
                let identity_name: String = c
                    .query_one(
                        "SELECT name FROM runbook.user_identity WHERE user_id = $1",
                        &[&u.id.as_str()],
                    )
                    .await
                    .map_err(db)?
                    .get(0);
                if identity_name != u.name {
                    return Err(DirectoryError::Duplicate);
                }
                c.execute(
                    "INSERT INTO runbook.org_user (org_id, user_id, name, active, org_role) \
                     VALUES ($1, $2, $3, $4, $5)",
                    &[
                        &org,
                        &u.id.as_str(),
                        &u.name,
                        &u.active,
                        &org_role_label(u.role),
                    ],
                )
                .await
                .map_err(db)?;
            }
            DirectoryMutation::CreateProject(p) => {
                if p.org_id != commit.org_id {
                    return Err(DirectoryError::Invalid("organization mismatch"));
                }
                if !slug(&p.slug) || p.title.trim().is_empty() {
                    return Err(DirectoryError::Invalid("project"));
                }
                let duplicate: bool = c
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM runbook.project \
                         WHERE project_id = $1 OR (org_id = $2 AND slug = $3))",
                        &[&p.id.as_str(), &org, &p.slug],
                    )
                    .await
                    .map_err(db)?
                    .get(0);
                if duplicate {
                    return Err(DirectoryError::Duplicate);
                }
                c.execute(
                    "INSERT INTO runbook.project (project_id, org_id, slug, title) \
                     VALUES ($1, $2, $3, $4)",
                    &[&p.id.as_str(), &org, &p.slug, &p.title],
                )
                .await
                .map_err(db)?;
            }
            DirectoryMutation::CreateTeam(t) => {
                if t.org_id != commit.org_id {
                    return Err(DirectoryError::Invalid("organization mismatch"));
                }
                if !slug(&t.slug) || t.title.trim().is_empty() {
                    return Err(DirectoryError::Invalid("team"));
                }
                let duplicate: bool = c
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM runbook.team \
                         WHERE team_id = $1 OR (org_id = $2 AND slug = $3))",
                        &[&t.id.as_str(), &org, &t.slug],
                    )
                    .await
                    .map_err(db)?
                    .get(0);
                if duplicate {
                    return Err(DirectoryError::Duplicate);
                }
                c.execute(
                    "INSERT INTO runbook.team (team_id, org_id, slug, title) \
                     VALUES ($1, $2, $3, $4)",
                    &[&t.id.as_str(), &org, &t.slug, &t.title],
                )
                .await
                .map_err(db)?;
            }
            DirectoryMutation::SetOrgRole { user_id, role } => {
                let updated = c
                    .execute(
                        "UPDATE runbook.org_user SET org_role = $3 WHERE org_id = $1 AND user_id = $2",
                        &[&org, &user_id.as_str(), &org_role_label(role)],
                    )
                    .await
                    .map_err(db)?;
                if updated == 0 {
                    return Err(DirectoryError::NotFound);
                }
            }
            DirectoryMutation::SetProjectTitle { project_id, title } => {
                if title.trim().is_empty() {
                    return Err(DirectoryError::Invalid("project"));
                }
                let updated = c
                    .execute(
                        "UPDATE runbook.project SET title = $3 \
                         WHERE org_id = $1 AND project_id = $2",
                        &[&org, &project_id.as_str(), &title],
                    )
                    .await
                    .map_err(db)?;
                if updated == 0 {
                    return Err(DirectoryError::NotFound);
                }
            }
            DirectoryMutation::SetUserName { user_id, name } => {
                if name.trim().is_empty() {
                    return Err(DirectoryError::Invalid("user name"));
                }
                if !org_user_exists(c, org, &user_id).await? {
                    return Err(DirectoryError::NotFound);
                }
                // One identity: its name and every membership's copy of it.
                for sql in [
                    "UPDATE runbook.user_identity SET name = $2 WHERE user_id = $1",
                    "UPDATE runbook.org_user SET name = $2 WHERE user_id = $1",
                ] {
                    c.execute(sql, &[&user_id.as_str(), &name])
                        .await
                        .map_err(db)?;
                }
            }
            DirectoryMutation::SetUserActive { user_id, active } => {
                let updated = c
                    .execute(
                        "UPDATE runbook.org_user SET active = $3 \
                         WHERE org_id = $1 AND user_id = $2",
                        &[&org, &user_id.as_str(), &active],
                    )
                    .await
                    .map_err(db)?;
                if updated == 0 {
                    return Err(DirectoryError::NotFound);
                }
            }
            DirectoryMutation::GrantProjectRole {
                project_id,
                user_id,
                role,
            } => {
                if !project_in_org(c, org, &project_id).await?
                    || !org_user_exists(c, org, &user_id).await?
                {
                    return Err(DirectoryError::NotFound);
                }
                c.execute(
                    "INSERT INTO runbook.project_member (org_id, project_id, user_id, role) \
                     VALUES ($1, $2, $3, $4) ON CONFLICT (project_id, user_id) \
                     DO UPDATE SET role = EXCLUDED.role",
                    &[
                        &org,
                        &project_id.as_str(),
                        &user_id.as_str(),
                        &role_label(role),
                    ],
                )
                .await
                .map_err(db)?;
            }
            DirectoryMutation::RevokeProjectRole {
                project_id,
                user_id,
            } => {
                if !project_in_org(c, org, &project_id).await?
                    || !org_user_exists(c, org, &user_id).await?
                {
                    return Err(DirectoryError::NotFound);
                }
                c.execute(
                    "DELETE FROM runbook.project_member \
                     WHERE org_id = $1 AND project_id = $2 AND user_id = $3",
                    &[&org, &project_id.as_str(), &user_id.as_str()],
                )
                .await
                .map_err(db)?;
            }
            DirectoryMutation::AddTeamMember { team_id, user_id } => {
                if !team_in_org(c, org, &team_id).await?
                    || !org_user_exists(c, org, &user_id).await?
                {
                    return Err(DirectoryError::NotFound);
                }
                c.execute(
                    "INSERT INTO runbook.team_member (org_id, team_id, user_id) \
                     VALUES ($1, $2, $3) ON CONFLICT (team_id, user_id) DO NOTHING",
                    &[&org, &team_id.as_str(), &user_id.as_str()],
                )
                .await
                .map_err(db)?;
            }
            DirectoryMutation::RemoveTeamMember { team_id, user_id } => {
                if !team_in_org(c, org, &team_id).await?
                    || !org_user_exists(c, org, &user_id).await?
                {
                    return Err(DirectoryError::NotFound);
                }
                c.execute(
                    "DELETE FROM runbook.team_member \
                     WHERE org_id = $1 AND team_id = $2 AND user_id = $3",
                    &[&org, &team_id.as_str(), &user_id.as_str()],
                )
                .await
                .map_err(db)?;
            }
            DirectoryMutation::BindVerifiedEmail { user_id, email } => {
                if !org_user_exists(c, org, &user_id).await? {
                    return Err(DirectoryError::NotFound);
                }
                let key = email_key(&email)?;
                c.execute(
                    "INSERT INTO runbook.verified_email (email, user_id) VALUES ($1, $2) \
                     ON CONFLICT (email) DO NOTHING",
                    &[&key, &user_id.as_str()],
                )
                .await
                .map_err(db)?;
                let owner: String = c
                    .query_one(
                        "SELECT user_id FROM runbook.verified_email WHERE email = $1",
                        &[&key],
                    )
                    .await
                    .map_err(db)?
                    .get(0);
                if owner != user_id.as_str() {
                    return Err(DirectoryError::Duplicate);
                }
            }
        }
    }
    if had_owner && !has_active_owner(c, org).await? {
        return Err(DirectoryError::Invalid("last owner"));
    }
    c.execute(
        "UPDATE runbook.directory_revision SET revision = $2 WHERE org_id = $1",
        &[&org, &int(revision)?],
    )
    .await
    .map_err(db)?;
    Ok(CommitResult::Committed(revision))
}

/// The organization's directory snapshot inside the caller's transaction.
pub(crate) async fn snapshot_in(
    c: &Client,
    org_id: &OrganizationId,
) -> Result<DirectorySnapshot, DirectoryError> {
    let org = org_id.as_str();
    let title: String = c
        .query_opt(
            "SELECT title FROM runbook.organization WHERE org_id = $1",
            &[&org],
        )
        .await
        .map_err(db)?
        .ok_or(DirectoryError::NotFound)?
        .get(0);
    let revision = uint(
        c.query_one(
            "SELECT revision FROM runbook.directory_revision WHERE org_id = $1",
            &[&org],
        )
        .await
        .map_err(db)?
        .get(0),
    )?;
    let mut users = BTreeMap::new();
    for row in c
        .query(
            "SELECT user_id, name, active, org_role FROM runbook.org_user WHERE org_id = $1",
            &[&org],
        )
        .await
        .map_err(db)?
    {
        let user = UserRecord {
            id: parse(row.get(0))?,
            name: row.get(1),
            active: row.get(2),
            role: org_role_of(row.get(3))?,
        };
        users.insert(user.id.clone(), user);
    }
    let mut projects = BTreeMap::new();
    for row in c
        .query(
            "SELECT project_id, slug, title FROM runbook.project WHERE org_id = $1",
            &[&org],
        )
        .await
        .map_err(db)?
    {
        let project = ProjectRecord {
            id: parse(row.get(0))?,
            org_id: org_id.clone(),
            slug: row.get(1),
            title: row.get(2),
        };
        projects.insert(project.id.clone(), project);
    }
    let mut teams = BTreeMap::new();
    for row in c
        .query(
            "SELECT team_id, slug, title FROM runbook.team WHERE org_id = $1",
            &[&org],
        )
        .await
        .map_err(db)?
    {
        let team = TeamRecord {
            id: parse(row.get(0))?,
            org_id: org_id.clone(),
            slug: row.get(1),
            title: row.get(2),
        };
        teams.insert(team.id.clone(), team);
    }
    let mut project_members = BTreeMap::new();
    for row in c
        .query(
            "SELECT project_id, user_id, role FROM runbook.project_member \
                 WHERE org_id = $1",
            &[&org],
        )
        .await
        .map_err(db)?
    {
        project_members.insert(
            (parse(row.get(0))?, parse(row.get(1))?),
            role_of(row.get(2))?,
        );
    }
    let mut team_members = BTreeSet::new();
    for row in c
        .query(
            "SELECT team_id, user_id FROM runbook.team_member WHERE org_id = $1",
            &[&org],
        )
        .await
        .map_err(db)?
    {
        team_members.insert((parse(row.get(0))?, parse(row.get(1))?));
    }
    Ok(DirectorySnapshot {
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
    })
}

impl runbook_core::pictures::PictureStore for PostgresDirectory {
    async fn picture(
        &self,
        owner: &runbook_core::pictures::PictureOwner,
    ) -> Result<Option<runbook_core::pictures::Picture>, DirectoryError> {
        use runbook_core::pictures::{Picture, PictureOwner};
        let conn = self.pool.get().await.map_err(connection)?;
        let row = match owner {
            PictureOwner::User(user) => conn
                .client()
                .query_opt(
                    "SELECT content_type, bytes, source_url, updated_at \
                     FROM runbook.user_picture WHERE user_id = $1",
                    &[&user.as_str()],
                )
                .await
                .map_err(db)?,
            PictureOwner::Project(org, project) => conn
                .client()
                .query_opt(
                    "SELECT content_type, bytes, NULL::text, updated_at \
                     FROM runbook.project_picture WHERE org_id = $1 AND project_id = $2",
                    &[&org.as_str(), &project.as_str()],
                )
                .await
                .map_err(db)?,
        };
        Ok(row.map(|row| Picture {
            content_type: row.get(0),
            bytes: row.get(1),
            source_url: row.get(2),
            updated_at_ms: row.get(3),
        }))
    }
    async fn set_picture(
        &self,
        owner: &runbook_core::pictures::PictureOwner,
        picture: Option<runbook_core::pictures::Picture>,
    ) -> Result<(), DirectoryError> {
        use runbook_core::pictures::PictureOwner;
        let conn = self.pool.get().await.map_err(connection)?;
        let client = conn.client();
        match (owner, picture) {
            (PictureOwner::User(user), Some(p)) => client
                .execute(
                    "INSERT INTO runbook.user_picture \
                     (user_id, content_type, bytes, source_url, updated_at) \
                     VALUES ($1, $2, $3, $4, $5) ON CONFLICT (user_id) DO UPDATE SET \
                     content_type = $2, bytes = $3, source_url = $4, updated_at = $5",
                    &[
                        &user.as_str(),
                        &p.content_type,
                        &p.bytes,
                        &p.source_url,
                        &p.updated_at_ms,
                    ],
                )
                .await
                .map_err(db)?,
            (PictureOwner::User(user), None) => client
                .execute(
                    "DELETE FROM runbook.user_picture WHERE user_id = $1",
                    &[&user.as_str()],
                )
                .await
                .map_err(db)?,
            (PictureOwner::Project(org, project), Some(p)) => client
                .execute(
                    "INSERT INTO runbook.project_picture \
                     (org_id, project_id, content_type, bytes, updated_at) \
                     VALUES ($1, $2, $3, $4, $5) ON CONFLICT (org_id, project_id) DO UPDATE SET \
                     content_type = $3, bytes = $4, updated_at = $5",
                    &[
                        &org.as_str(),
                        &project.as_str(),
                        &p.content_type,
                        &p.bytes,
                        &p.updated_at_ms,
                    ],
                )
                .await
                .map_err(db)?,
            (PictureOwner::Project(org, project), None) => client
                .execute(
                    "DELETE FROM runbook.project_picture WHERE org_id = $1 AND project_id = $2",
                    &[&org.as_str(), &project.as_str()],
                )
                .await
                .map_err(db)?,
        };
        Ok(())
    }
}
impl DirectoryStore for PostgresDirectory {
    async fn snapshot(&self, org_id: &OrganizationId) -> Result<DirectorySnapshot, DirectoryError> {
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(SNAPSHOT_READ).await.map_err(db)?;
        let result = snapshot_in(conn.client(), org_id).await;
        finish(&mut conn, result).await
    }

    async fn commit(&self, commit: DirectoryCommit) -> Result<CommitResult, DirectoryError> {
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(READ_COMMITTED).await.map_err(db)?;
        let result = apply(conn.client(), commit).await;
        finish_commit(&mut conn, result).await
    }

    async fn user_by_id(&self, user_id: &UserId) -> Result<Option<UserIdentity>, DirectoryError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT name FROM runbook.user_identity WHERE user_id = $1",
                &[&user_id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|row| {
                Ok(UserIdentity {
                    id: user_id.clone(),
                    name: row.get(0),
                })
            })
            .transpose()
    }

    async fn user_by_verified_email(
        &self,
        email: &str,
    ) -> Result<Option<UserIdentity>, DirectoryError> {
        let key = email_key(email)?;
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT i.user_id, i.name FROM runbook.verified_email e \
                 JOIN runbook.user_identity i ON i.user_id = e.user_id WHERE e.email = $1",
                &[&key],
            )
            .await
            .map_err(db)?
            .map(|row| {
                Ok(UserIdentity {
                    id: parse(row.get(0))?,
                    name: row.get(1),
                })
            })
            .transpose()
    }

    async fn verified_emails(
        &self,
        org_id: &OrganizationId,
    ) -> Result<std::collections::BTreeMap<UserId, String>, DirectoryError> {
        let conn = self.pool.get().await.map_err(connection)?;
        if conn
            .client()
            .query_opt(
                "SELECT org_id FROM runbook.organization WHERE org_id = $1",
                &[&org_id.as_str()],
            )
            .await
            .map_err(db)?
            .is_none()
        {
            return Err(DirectoryError::NotFound);
        }
        let rows = conn
            .client()
            .query(
                "SELECT e.user_id, e.email FROM runbook.verified_email e \
                 JOIN runbook.org_user ou ON ou.user_id = e.user_id WHERE ou.org_id = $1",
                &[&org_id.as_str()],
            )
            .await
            .map_err(db)?;
        rows.iter()
            .map(|row| Ok((parse(row.get(0))?, row.get(1))))
            .collect()
    }

    async fn org_for_project(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<OrganizationId>, DirectoryError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT org_id FROM runbook.project WHERE project_id = $1",
                &[&project_id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|row| parse(row.get(0)))
            .transpose()
    }
}

mod auth_links;
pub(crate) mod operations;

mod recovery;
pub use recovery::{OwnerAssignment, OwnerRecoveryFault};
