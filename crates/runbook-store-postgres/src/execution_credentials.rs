//! B7a execution credential rows: writes ride execution commits so audit
//! events and token state land atomically (including reuse revocation).
use crate::{
    PostgresExecutionStore,
    rows::{connection, db, decode, encode, int},
};
use runbook_core::{
    StoreError,
    credentials::SecretHash,
    execution_credentials::{
        CredentialFamily, CredentialWrite, ExecutionCredentialRecord, ExecutionCredentialStore,
    },
};
use runbook_model::{ExecutionId, TokenId};
use tokio_postgres::Client;

/// F33: every row a write creates names the commit's run, tenant and family.
fn bound(
    scope: &runbook_core::catalog::TenantScope,
    execution_id: &ExecutionId,
    family_id: &str,
    records: [&ExecutionCredentialRecord; 2],
) -> Result<(), StoreError> {
    if records.iter().all(|r| {
        &r.execution_id == execution_id
            && r.org_id == scope.org_id
            && r.project_id == scope.project_id
            && r.family_id == family_id
    }) {
        Ok(())
    } else {
        Err(StoreError::InvalidBatch("credentials.foreign_execution"))
    }
}

/// Checks one credential write against the store before anything is
/// written. Writes are bound to the commit's own run (`execution_id`) and
/// tenant: a token or family of another run reads as absent (F33).
pub(crate) async fn precheck(
    c: &Client,
    scope: &runbook_core::catalog::TenantScope,
    execution_id: &ExecutionId,
    write: &CredentialWrite,
) -> Result<(), StoreError> {
    match write {
        CredentialWrite::IssueFamily {
            family,
            access,
            refresh,
        } => {
            if &family.execution_id != execution_id
                || family.org_id != scope.org_id
                || family.project_id != scope.project_id
            {
                return Err(StoreError::InvalidBatch("credentials.foreign_execution"));
            }
            bound(scope, execution_id, &family.family_id, [access, refresh])?;
            let existing = c
                .query_opt(
                    "SELECT request_hash FROM runbook.credential_family \
                     WHERE org_id = $1 AND project_id = $2 AND execution_id = $3 \
                     AND idempotency_key = $4",
                    &[
                        &scope.org_id.as_str(),
                        &scope.project_id.as_str(),
                        &family.execution_id.as_str(),
                        &family.idempotency_key,
                    ],
                )
                .await
                .map_err(db)?;
            if let Some(row) = existing {
                let hash: Vec<u8> = row.get(0);
                return Err(StoreError::InvalidBatch(if hash == family.request_hash {
                    "credentials.already_issued"
                } else {
                    "request.idempotency_key_reused"
                }));
            }
        }
        CredentialWrite::Rotate {
            consume_token_id,
            access,
            refresh,
            ..
        } => {
            let row = c
                .query_opt(
                    "SELECT consumed_at_ms, revoked_at_ms, kind, family_id \
                     FROM runbook.execution_credential WHERE token_id = $1 \
                     AND execution_id = $2 AND org_id = $3 AND project_id = $4 FOR UPDATE",
                    &[
                        &consume_token_id.as_str(),
                        &execution_id.as_str(),
                        &scope.org_id.as_str(),
                        &scope.project_id.as_str(),
                    ],
                )
                .await
                .map_err(db)?
                .ok_or(StoreError::NotFound)?;
            bound(scope, execution_id, row.get(3), [access, refresh])?;
            if row.get::<_, Option<i64>>(0).is_some() {
                return Err(StoreError::InvalidBatch("credentials.refresh_reused"));
            }
            if row.get::<_, Option<i64>>(1).is_some() || row.get::<_, &str>(2) != "refresh" {
                return Err(StoreError::InvalidBatch("credentials.generation_conflict"));
            }
        }
        CredentialWrite::RevokeFamily {
            execution_id: revoked,
            family_id,
            ..
        } => {
            if revoked != execution_id {
                return Err(StoreError::InvalidBatch("credentials.foreign_execution"));
            }
            let exists: bool = c
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM runbook.credential_family \
                     WHERE org_id = $1 AND project_id = $2 AND family_id = $3 \
                     AND execution_id = $4)",
                    &[
                        &scope.org_id.as_str(),
                        &scope.project_id.as_str(),
                        family_id,
                        &execution_id.as_str(),
                    ],
                )
                .await
                .map_err(db)?
                .get(0);
            if !exists {
                return Err(StoreError::NotFound);
            }
        }
    }
    Ok(())
}

async fn insert_record(c: &Client, record: &ExecutionCredentialRecord) -> Result<(), StoreError> {
    let kind = match record.kind {
        runbook_core::credentials::TokenKind::Execution => "execution",
        runbook_core::credentials::TokenKind::Refresh => "refresh",
        _ => return Err(StoreError::InvalidBatch("credentials.invalid_kind")),
    };
    c.execute(
        "INSERT INTO runbook.execution_credential \
         (token_id, kind, hash, family_id, execution_id, org_id, project_id, generation, \
          issued_at_ms, expires_at_ms, consumed_at_ms, revoked_at_ms, body) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        &[
            &record.token_id.as_str(),
            &kind,
            &record.hash.as_bytes().as_slice(),
            &record.family_id,
            &record.execution_id.as_str(),
            &record.org_id.as_str(),
            &record.project_id.as_str(),
            &int(record.generation)?,
            &record.issued_at_ms,
            &record.expires_at_ms,
            &record.consumed_at_ms,
            &record.revoked_at_ms,
            &encode(record)?,
        ],
    )
    .await
    .map_err(|error| {
        if crate::rows::is_unique_violation(&error) {
            StoreError::InvalidBatch("credentials.duplicate")
        } else {
            db(error)
        }
    })?;
    Ok(())
}

pub(crate) async fn apply(c: &Client, write: CredentialWrite) -> Result<(), StoreError> {
    match write {
        CredentialWrite::IssueFamily {
            family,
            access,
            refresh,
        } => {
            c.execute(
                "INSERT INTO runbook.credential_family \
                 (family_id, execution_id, org_id, project_id, issuer_token_id, body, \
                  idempotency_key, request_hash, created_at_ms, revoked_at_ms) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                &[
                    &family.family_id,
                    &family.execution_id.as_str(),
                    &family.org_id.as_str(),
                    &family.project_id.as_str(),
                    &family.issuer_token_id.as_str(),
                    &encode(&family)?,
                    &family.idempotency_key,
                    &family.request_hash.as_slice(),
                    &family.created_at_ms,
                    &family.revoked_at_ms,
                ],
            )
            .await
            .map_err(|error| {
                if crate::rows::is_unique_violation(&error) {
                    StoreError::InvalidBatch("credentials.duplicate")
                } else {
                    db(error)
                }
            })?;
            insert_record(c, &access).await?;
            insert_record(c, &refresh).await
        }
        CredentialWrite::Rotate {
            consume_token_id,
            consumed_at_ms,
            access,
            refresh,
        } => {
            c.execute(
                "UPDATE runbook.execution_credential SET consumed_at_ms = $2, \
                 body = jsonb_set(body::jsonb, '{consumed_at_ms}', to_jsonb($2::bigint))::text \
                 WHERE token_id = $1",
                &[&consume_token_id.as_str(), &consumed_at_ms],
            )
            .await
            .map_err(db)?;
            insert_record(c, &access).await?;
            insert_record(c, &refresh).await
        }
        CredentialWrite::RevokeFamily {
            execution_id,
            family_id,
            revoked_at_ms,
        } => revoke_family(c, &execution_id, &family_id, revoked_at_ms).await,
    }
}

pub(crate) async fn revoke_family(
    c: &Client,
    execution_id: &ExecutionId,
    family_id: &str,
    at_ms: i64,
) -> Result<(), StoreError> {
    c.execute(
        "UPDATE runbook.credential_family SET revoked_at_ms = $3, \
         body = jsonb_set(body::jsonb, '{revoked_at_ms}', to_jsonb($3::bigint))::text \
         WHERE execution_id = $1 AND family_id = $2 AND revoked_at_ms IS NULL",
        &[&execution_id.as_str(), &family_id, &at_ms],
    )
    .await
    .map_err(db)?;
    c.execute(
        "UPDATE runbook.execution_credential SET revoked_at_ms = $3, \
         body = jsonb_set(body::jsonb, '{revoked_at_ms}', to_jsonb($3::bigint))::text \
         WHERE execution_id = $1 AND family_id = $2 AND revoked_at_ms IS NULL",
        &[&execution_id.as_str(), &family_id, &at_ms],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// Terminal closure revokes every family of the run in the same transaction.
pub(crate) async fn revoke_all(
    c: &Client,
    execution_id: &ExecutionId,
    at_ms: i64,
) -> Result<(), StoreError> {
    let rows = c
        .query(
            "SELECT family_id FROM runbook.credential_family WHERE execution_id = $1",
            &[&execution_id.as_str()],
        )
        .await
        .map_err(db)?;
    for row in rows {
        revoke_family(c, execution_id, row.get(0), at_ms).await?;
    }
    Ok(())
}

const FAMILY_SELECT: &str = "SELECT body FROM runbook.credential_family";
impl ExecutionCredentialStore for PostgresExecutionStore {
    async fn credential_family(
        &self,
        family_id: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                &format!(
                    "{FAMILY_SELECT} WHERE org_id = $1 AND project_id = $2 AND family_id = $3"
                ),
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &family_id,
                ],
            )
            .await
            .map_err(db)?
            .map(|row| decode(row.get(0)))
            .transpose()
    }
    async fn credential_by_hash(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT body FROM runbook.execution_credential \
                 WHERE org_id = $1 AND project_id = $2 AND hash = $3",
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &hash.as_bytes().as_slice(),
                ],
            )
            .await
            .map_err(db)?
            .map(|row| decode(row.get(0)))
            .transpose()
    }
    async fn credential_by_id(
        &self,
        token_id: &TokenId,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT body FROM runbook.execution_credential \
                 WHERE org_id = $1 AND project_id = $2 AND token_id = $3",
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &token_id.as_str(),
                ],
            )
            .await
            .map_err(db)?
            .map(|row| decode(row.get(0)))
            .transpose()
    }
    async fn family_by_idempotency_key(
        &self,
        execution_id: &ExecutionId,
        key: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                &format!(
                    "{FAMILY_SELECT} WHERE org_id = $1 AND project_id = $2 \
                     AND execution_id = $3 AND idempotency_key = $4"
                ),
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &execution_id.as_str(),
                    &key,
                ],
            )
            .await
            .map_err(db)?
            .map(|row| decode(row.get(0)))
            .transpose()
    }
}
