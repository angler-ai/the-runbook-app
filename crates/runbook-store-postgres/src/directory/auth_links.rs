//! AUTH2 (D45) links and invitations on PostgreSQL. Linking locks the
//! organization's directory revision row, so the link, the directory commit
//! and the invitation's acceptance land together or not at all.
use super::*;
use runbook_core::auth_links::{
    AuthLink, AuthLinkStore, InvitationRecord, InvitationState, LinkPlan, LinkRequest, LinkState,
    plan_link, validate_invitation,
};
use runbook_model::InvitationId;

fn state_of(text: &str) -> Result<InvitationState, DirectoryError> {
    match text {
        "pending" => Ok(InvitationState::Pending),
        "accepted" => Ok(InvitationState::Accepted),
        "revoked" => Ok(InvitationState::Revoked),
        _ => Err(DirectoryError::Backend("postgres.invalid_invitation")),
    }
}
const INVITATION_COLUMNS: &str = "invitation_id, org_id, email, role, projects, invited_by, \
     created_at, expires_at, state, accepted_by, closed_at, name";
fn invitation(row: &tokio_postgres::Row) -> Result<InvitationRecord, DirectoryError> {
    Ok(InvitationRecord {
        id: parse(row.get(0))?,
        org_id: parse(row.get(1))?,
        email: row.get(2),
        role: org_role_of(row.get(3))?,
        projects: serde_json::from_str(row.get(4))
            .map_err(|_| DirectoryError::Backend("postgres.invalid_invitation"))?,
        invited_by: parse(row.get(5))?,
        created_at_ms: row.get(6),
        expires_at_ms: row.get(7),
        state: state_of(row.get(8))?,
        accepted_by: row.get::<_, Option<&str>>(9).map(parse).transpose()?,
        closed_at_ms: row.get(10),
        name: row.get(11),
    })
}
fn link(row: &tokio_postgres::Row) -> Result<AuthLink, DirectoryError> {
    Ok(AuthLink {
        auth_user_id: row.get(0),
        org_id: parse(row.get(1))?,
        user_id: parse(row.get(2))?,
        email: row.get(3),
        linked_at_ms: row.get(4),
    })
}
const LINK_COLUMNS: &str = "auth_user_id, org_id, user_id, email, linked_at";

async fn link_in(
    c: &Client,
    org: &OrganizationId,
    request: &LinkRequest,
) -> Result<AuthLink, DirectoryError> {
    let revision = uint(
        c.query_opt(
            "SELECT revision FROM runbook.directory_revision WHERE org_id = $1 FOR UPDATE",
            &[&org.as_str()],
        )
        .await
        .map_err(db)?
        .ok_or(DirectoryError::NotFound)?
        .get(0),
    )?;
    let existing = c
        .query_opt(
            &format!("SELECT {LINK_COLUMNS} FROM runbook.auth_link WHERE auth_user_id = $1"),
            &[&request.auth_user_id],
        )
        .await
        .map_err(db)?
        .map(|row| link(&row))
        .transpose()?;
    let named = match &request.invitation {
        Some(id) => c
            .query_opt(
                &format!(
                    "SELECT {INVITATION_COLUMNS} FROM runbook.invitation \
                     WHERE invitation_id = $1 FOR UPDATE"
                ),
                &[&id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|row| invitation(&row))
            .transpose()?,
        None => None,
    };
    let email = email_key(&request.verified_email)?;
    let owner: Option<UserId> = c
        .query_opt(
            "SELECT user_id FROM runbook.verified_email WHERE email = $1",
            &[&email],
        )
        .await
        .map_err(db)?
        .map(|row| parse(row.get(0)))
        .transpose()?;
    let identity_name: Option<String> = c
        .query_opt(
            "SELECT name FROM runbook.user_identity WHERE user_id = $1",
            &[&request.user_id.as_str()],
        )
        .await
        .map_err(db)?
        .map(|row| row.get(0));
    let snapshot = snapshot_in(c, org).await?;
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
        LinkPlan::Existing(link) => return Ok(link),
        LinkPlan::Commit {
            mutations,
            link,
            accept,
        } => (mutations, link, accept),
    };
    if apply(
        c,
        DirectoryCommit {
            org_id: org.clone(),
            expected_revision: revision,
            mutations,
        },
    )
    .await?
        == CommitResult::Conflict
    {
        return Err(DirectoryError::Backend("postgres.retry_transaction"));
    }
    if let Some(id) = accept {
        let updated = c
            .execute(
                "UPDATE runbook.invitation SET state = 'accepted', accepted_by = $2, closed_at = $3 \
                 WHERE invitation_id = $1 AND state = 'pending'",
                &[&id.as_str(), &request.user_id.as_str(), &request.now_ms],
            )
            .await
            .map_err(db)?;
        if updated != 1 {
            return Err(DirectoryError::Invalid("invitation"));
        }
    }
    c.execute(
        "INSERT INTO runbook.auth_link (auth_user_id, org_id, user_id, email, linked_at) \
         VALUES ($1, $2, $3, $4, $5)",
        &[
            &new_link.auth_user_id,
            &new_link.org_id.as_str(),
            &new_link.user_id.as_str(),
            &new_link.email,
            &new_link.linked_at_ms,
        ],
    )
    .await
    .map_err(db)?;
    Ok(new_link)
}

async fn create_in(
    c: &Client,
    org: &OrganizationId,
    record: &InvitationRecord,
) -> Result<(), DirectoryError> {
    // Serialize invitations of one organization behind its revision row.
    c.query_opt(
        "SELECT revision FROM runbook.directory_revision WHERE org_id = $1 FOR UPDATE",
        &[&org.as_str()],
    )
    .await
    .map_err(db)?
    .ok_or(DirectoryError::NotFound)?;
    let snapshot = snapshot_in(c, org).await?;
    let record = validate_invitation(org, record, &snapshot)?;
    let duplicate: bool = c
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM runbook.invitation WHERE org_id = $1 AND email = $2 \
             AND state = 'pending' AND expires_at > $3)",
            &[&org.as_str(), &record.email, &record.created_at_ms],
        )
        .await
        .map_err(db)?
        .get(0);
    if duplicate {
        return Err(DirectoryError::Duplicate);
    }
    let projects =
        serde_json::to_string(&record.projects).map_err(|_| DirectoryError::Invalid("projects"))?;
    c.execute(
        "INSERT INTO runbook.invitation (invitation_id, org_id, email, role, projects, \
         invited_by, created_at, expires_at, state, name) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'pending',$9)",
        &[
            &record.id.as_str(),
            &org.as_str(),
            &record.email,
            &org_role_label(record.role),
            &projects,
            &record.invited_by.as_str(),
            &record.created_at_ms,
            &record.expires_at_ms,
            &record.name,
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}

impl AuthLinkStore for PostgresDirectory {
    async fn auth_link(&self, auth_user_id: &str) -> Result<Option<AuthLink>, DirectoryError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                &format!("SELECT {LINK_COLUMNS} FROM runbook.auth_link WHERE auth_user_id = $1"),
                &[&auth_user_id],
            )
            .await
            .map_err(db)?
            .map(|row| link(&row))
            .transpose()
    }
    async fn auth_users_of(&self, user_id: &UserId) -> Result<Vec<String>, DirectoryError> {
        let conn = self.pool.get().await.map_err(connection)?;
        Ok(conn
            .client()
            .query(
                "SELECT auth_user_id FROM runbook.auth_link WHERE user_id = $1 ORDER BY auth_user_id",
                &[&user_id.as_str()],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|row| row.get::<_, String>(0))
            .collect())
    }
    async fn invitations_for_email(
        &self,
        email: &str,
        now_ms: i64,
    ) -> Result<Vec<InvitationRecord>, DirectoryError> {
        let key = email_key(email)?;
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query(
                &format!(
                    "SELECT {INVITATION_COLUMNS} FROM runbook.invitation \
                     WHERE email = $1 AND state = 'pending' AND expires_at > $2 \
                     ORDER BY created_at DESC, invitation_id DESC"
                ),
                &[&key, &now_ms],
            )
            .await
            .map_err(db)?
            .iter()
            .map(invitation)
            .collect()
    }
    async fn link_identity(
        &self,
        org: &OrganizationId,
        request: LinkRequest,
    ) -> Result<AuthLink, DirectoryError> {
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(READ_COMMITTED).await.map_err(db)?;
        let result = link_in(conn.client(), org, &request).await;
        finish(&mut conn, result).await
    }
    async fn create_invitation(
        &self,
        org: &OrganizationId,
        invitation: InvitationRecord,
    ) -> Result<(), DirectoryError> {
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(READ_COMMITTED).await.map_err(db)?;
        let result = create_in(conn.client(), org, &invitation).await;
        finish(&mut conn, result).await
    }
    async fn list_invitations(
        &self,
        org: &OrganizationId,
    ) -> Result<Vec<InvitationRecord>, DirectoryError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query(
                &format!(
                    "SELECT {INVITATION_COLUMNS} FROM runbook.invitation WHERE org_id = $1 \
                     ORDER BY created_at DESC, invitation_id DESC"
                ),
                &[&org.as_str()],
            )
            .await
            .map_err(db)?
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
        let conn = self.pool.get().await.map_err(connection)?;
        Ok(conn
            .client()
            .execute(
                "UPDATE runbook.invitation SET state = 'revoked', closed_at = $3 \
                 WHERE org_id = $1 AND invitation_id = $2 AND state = 'pending'",
                &[&org.as_str(), &id.as_str(), &now_ms],
            )
            .await
            .map_err(db)?
            == 1)
    }
}

impl PostgresDirectory {
    /// Organizations where the user is an active member, by ID. A global key
    /// read for linking a session with no invitation (D45); grants nothing.
    pub async fn active_orgs_of_user(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<OrganizationId>, DirectoryError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query(
                "SELECT org_id FROM runbook.org_user WHERE user_id = $1 AND active ORDER BY org_id",
                &[&user_id.as_str()],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|row| parse(row.get(0)))
            .collect()
    }
}
