//! Local administrative recovery, never an HTTP authorization bypass.
use super::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerAssignment {
    Assigned,
    AlreadyAssigned,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerRecoveryFault {
    None,
    BeforeAudit,
    AfterAudit,
}
impl PostgresDirectory {
    pub async fn assign_owner(
        &self,
        org: &OrganizationId,
        user: &UserId,
        fault: OwnerRecoveryFault,
    ) -> Result<OwnerAssignment, DirectoryError> {
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(READ_COMMITTED).await.map_err(db)?;
        let result = assign(conn.client(), org, user, fault).await;
        finish(&mut conn, result).await
    }
}
async fn assign(
    c: &Client,
    org: &OrganizationId,
    user: &UserId,
    fault: OwnerRecoveryFault,
) -> Result<OwnerAssignment, DirectoryError> {
    let revision: i64 = c
        .query_opt(
            "SELECT revision FROM runbook.directory_revision WHERE org_id=$1 FOR UPDATE",
            &[&org.as_str()],
        )
        .await
        .map_err(db)?
        .ok_or(DirectoryError::NotFound)?
        .get(0);
    let target = c
        .query_opt(
            "SELECT org_role FROM runbook.org_user WHERE org_id=$1 AND user_id=$2 AND active",
            &[&org.as_str(), &user.as_str()],
        )
        .await
        .map_err(db)?
        .ok_or(DirectoryError::Invalid(
            "owner recovery target not active member",
        ))?;
    let old_role: &str = target.get(0);
    if has_active_owner(c, org.as_str()).await? {
        let prior:bool=c.query_one("SELECT EXISTS (SELECT 1 FROM runbook.owner_recovery_audit WHERE org_id=$1 AND user_id=$2)",&[&org.as_str(),&user.as_str()]).await.map_err(db)?.get(0);
        return if old_role == "owner" && prior {
            Ok(OwnerAssignment::AlreadyAssigned)
        } else {
            Err(DirectoryError::Invalid("owner exists"))
        };
    }
    let revision = uint(revision)?;
    let result = apply(
        c,
        DirectoryCommit {
            org_id: org.clone(),
            expected_revision: revision,
            mutations: vec![DirectoryMutation::SetOrgRole {
                user_id: user.clone(),
                role: OrgRole::Owner,
            }],
        },
    )
    .await?;
    let next = match result {
        CommitResult::Committed(n) => n,
        CommitResult::Conflict => {
            return Err(DirectoryError::Backend("owner recovery revision conflict"));
        }
    };
    if fault == OwnerRecoveryFault::BeforeAudit {
        return Err(DirectoryError::Backend(
            "injected.owner_recovery_before_audit",
        ));
    }
    c.execute("INSERT INTO runbook.owner_recovery_audit(org_id,revision,user_id,from_role,to_role,actor,database_role) VALUES($1,$2,$3,$4,'owner','local_cli',current_user)",&[&org.as_str(),&int(next)?,&user.as_str(),&old_role]).await.map_err(db)?;
    if fault == OwnerRecoveryFault::AfterAudit {
        return Err(DirectoryError::Backend(
            "injected.owner_recovery_after_audit",
        ));
    }
    Ok(OwnerAssignment::Assigned)
}
