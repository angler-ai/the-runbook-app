//! Hash-only credential storage. Every mutation is one atomic SQL statement.
use crate::{DatabaseConfig, pool::Pool};
use runbook_core::credentials::*;
use runbook_model::{OrganizationId, TokenId};
use std::rc::Rc;
use tokio_postgres::error::SqlState;
mod device_codes;

#[derive(Clone)]
pub struct PostgresCredentials {
    pool: Rc<Pool>,
}
impl PostgresCredentials {
    /// Authentication bootstrap only: locate tenant by a presented secret hash.
    /// This grants no authority; callers must verify in the scoped execution store.
    pub async fn execution_scope(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<runbook_core::catalog::TenantScope>, CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        conn.client()
            .query_opt(
                "SELECT org_id, project_id FROM runbook.execution_credential WHERE hash=$1",
                &[&hash.as_bytes().as_slice()],
            )
            .await
            .map_err(db)?
            .map(|r| {
                Ok(runbook_core::catalog::TenantScope {
                    org_id: r.get::<_, &str>(0).parse().map_err(db)?,
                    project_id: r.get::<_, &str>(1).parse().map_err(db)?,
                })
            })
            .transpose()
    }
    pub fn new(config: DatabaseConfig) -> Self {
        Self {
            pool: Pool::new(config),
        }
    }
}
fn db(_: impl std::fmt::Debug) -> CredentialError {
    CredentialError::Backend("postgres.credentials")
}
impl CredentialStore for PostgresCredentials {
    async fn insert(&self, record: CredentialRecord) -> Result<(), CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        insert_on(conn.client(), &record).await
    }
    async fn find_by_hash(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<CredentialRecord>, CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        let row = conn.client().query_opt(
            "SELECT payload, revoked_at, last_used_at FROM runbook.credential WHERE secret_hash = $1", &[&&hash.as_bytes()[..]],
        ).await.map_err(db)?;
        row.map(|r| {
            let mut record: CredentialRecord = serde_json::from_str(r.get(0)).map_err(db)?;
            record.revoked_at = r.get(1);
            record.last_used_at = r.get(2);
            Ok(record)
        })
        .transpose()
    }
    async fn find_by_id(&self, id: &TokenId) -> Result<Option<CredentialRecord>, CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        let row = conn
            .client()
            .query_opt(
                "SELECT payload, revoked_at, last_used_at FROM runbook.credential WHERE id = $1",
                &[&id.as_str()],
            )
            .await
            .map_err(db)?;
        row.map(|r| {
            let mut record: CredentialRecord = serde_json::from_str(r.get(0)).map_err(db)?;
            record.revoked_at = r.get(1);
            record.last_used_at = r.get(2);
            Ok(record)
        })
        .transpose()
    }
    async fn revoke(
        &self,
        org: &OrganizationId,
        id: &TokenId,
        now: i64,
    ) -> Result<bool, CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        Ok(conn.client().execute(
            "UPDATE runbook.credential SET revoked_at = $3 WHERE org_id = $1 AND id = $2 AND revoked_at IS NULL",
            &[&org.as_str(), &id.as_str(), &now],
        ).await.map_err(db)? == 1)
    }
    async fn touch_last_used(
        &self,
        org: &OrganizationId,
        id: &TokenId,
        now: i64,
    ) -> Result<bool, CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        // NUMERIC subtraction handles the full i64 time domain without overflow.
        // PostgreSQL rechecks this predicate after any concurrent updater commits.
        Ok(conn.client().execute(
            "UPDATE runbook.credential SET last_used_at = $3 WHERE org_id = $1 AND id = $2 AND (last_used_at IS NULL OR last_used_at::numeric <= $3::bigint::numeric - 300000)",
            &[&org.as_str(), &id.as_str(), &now],
        ).await.map_err(db)? == 1)
    }
}

/// Reuse the same encoding/validation inside the local bootstrap transaction.
pub(crate) async fn insert_on(
    client: &tokio_postgres::Client,
    record: &CredentialRecord,
) -> Result<(), CredentialError> {
    record.validate()?;
    let payload = serde_json::to_string(record).map_err(db)?;
    client.execute(
        "INSERT INTO runbook.credential (id, org_id, secret_hash, payload, revoked_at, last_used_at) VALUES ($1,$2,$3,$4,$5,$6)",
        &[&record.id.as_str(), &record.org_id.as_str(), &&record.hash.as_bytes()[..], &payload, &record.revoked_at, &record.last_used_at],
    ).await.map_err(|e| if e.code() == Some(&SqlState::UNIQUE_VIOLATION) { CredentialError::Duplicate } else { db(e) })?;
    Ok(())
}

/// B4.1/C2 `/v1/tokens`: the D20 replay operation name in
/// `runbook.directory_request` (retained 24 hours like directory POSTs).
const ISSUE_OPERATION: &str = "POST:/v1/tokens";

/// One `POST /v1/tokens` attempt. The replay row stores the request body and
/// the token metadata view, never the secret.
pub struct TokenIssue {
    pub record: CredentialRecord,
    pub metadata: issuance::TokenMetadata,
    pub principal: String,
    pub key: String,
    pub payload: serde_json::Value,
    pub response: serde_json::Value,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueOutcome {
    Issued,
    /// Same principal, key and body: the secret was already shown once.
    AlreadyIssued,
    /// Same principal and key, different body.
    KeyReused,
}

impl PostgresCredentials {
    /// Read-only D20 probe before validating a retried request.
    pub async fn issue_replay(
        &self,
        org: &OrganizationId,
        principal: &str,
        key: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<IssueOutcome>, CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        replay(conn.client(), org, principal, key, payload).await
    }
    /// Insert the credential and its replay row in one transaction. A
    /// concurrent retry with the same key waits on the replay row and then
    /// answers from it, so a key never issues two secrets.
    pub async fn issue(&self, issue: &TokenIssue) -> Result<IssueOutcome, CredentialError> {
        let org = issue.record.org_id.as_str();
        let mut conn = self.pool.get().await.map_err(db)?;
        conn.begin(crate::execution::READ_COMMITTED)
            .await
            .map_err(db)?;
        let result = async {
            let c = conn.client();
            let now: i64 = c
                .query_one(
                    "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                    &[],
                )
                .await
                .map_err(db)?
                .get(0);
            c.execute(
                "DELETE FROM runbook.directory_request WHERE org_id=$1 AND operation=$2 AND expires_at<=$3",
                &[&org, &ISSUE_OPERATION, &now],
            )
            .await
            .map_err(db)?;
            let payload = serde_json::to_string(&issue.payload).map_err(db)?;
            let response = serde_json::to_string(&issue.response).map_err(db)?;
            let inserted = c
                .execute(
                    "INSERT INTO runbook.directory_request(org_id,principal,operation,key,payload,response,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",
                    &[&org, &issue.principal, &ISSUE_OPERATION, &issue.key, &payload, &response, &now.saturating_add(86_400_000)],
                )
                .await
                .map_err(db)?;
            if inserted == 0 {
                let org_id = &issue.record.org_id;
                return Ok(replay(c, org_id, &issue.principal, &issue.key, &issue.payload)
                    .await?
                    .unwrap_or(IssueOutcome::KeyReused));
            }
            insert_with_metadata(c, &issue.record, &issue.metadata).await?;
            Ok(IssueOutcome::Issued)
        }
        .await;
        match result {
            Ok(IssueOutcome::Issued) => {
                conn.commit().await.map_err(db)?;
                Ok(IssueOutcome::Issued)
            }
            other => {
                conn.rollback().await;
                other
            }
        }
    }
    /// D47: store a credential minted outside `/v1/tokens` (a device login)
    /// with its management metadata, so it is listed and revocable like any
    /// other personal token.
    pub async fn insert_issued(
        &self,
        record: &CredentialRecord,
        metadata: &issuance::TokenMetadata,
    ) -> Result<(), CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        insert_with_metadata(conn.client(), record, metadata).await
    }
    /// The organization's live credentials with their management metadata,
    /// ordered by ID. Execution credentials live elsewhere and never appear.
    pub async fn list_org(
        &self,
        org: &OrganizationId,
    ) -> Result<Vec<(CredentialRecord, issuance::TokenMetadata)>, CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        conn.client()
            .query(
                "SELECT payload, revoked_at, last_used_at FROM runbook.credential WHERE org_id = $1 AND revoked_at IS NULL ORDER BY id",
                &[&org.as_str()],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|r| {
                let payload: &str = r.get(0);
                let mut record: CredentialRecord = serde_json::from_str(payload).map_err(db)?;
                record.revoked_at = r.get(1);
                record.last_used_at = r.get(2);
                Ok((record, metadata_of(payload)?))
            })
            .collect()
    }
}

async fn replay(
    c: &tokio_postgres::Client,
    org: &OrganizationId,
    principal: &str,
    key: &str,
    payload: &serde_json::Value,
) -> Result<Option<IssueOutcome>, CredentialError> {
    let row = c
        .query_opt(
            "SELECT payload FROM runbook.directory_request WHERE org_id=$1 AND principal=$2 AND operation=$3 AND key=$4 AND expires_at > floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
            &[&org.as_str(), &principal, &ISSUE_OPERATION, &key],
        )
        .await
        .map_err(db)?;
    row.map(|r| {
        let saved: serde_json::Value = serde_json::from_str(r.get(0)).map_err(db)?;
        Ok(if &saved == payload {
            IssueOutcome::AlreadyIssued
        } else {
            IssueOutcome::KeyReused
        })
    })
    .transpose()
}

/// The metadata rides in the same JSON payload under `management`;
/// `CredentialRecord` ignores the key, so authentication is unchanged.
fn metadata_of(payload: &str) -> Result<issuance::TokenMetadata, CredentialError> {
    let value: serde_json::Value = serde_json::from_str(payload).map_err(db)?;
    match value.get("management") {
        Some(m) => serde_json::from_value(m.clone()).map_err(db),
        None => Ok(Default::default()),
    }
}

async fn insert_with_metadata(
    client: &tokio_postgres::Client,
    record: &CredentialRecord,
    metadata: &issuance::TokenMetadata,
) -> Result<(), CredentialError> {
    record.validate()?;
    let mut payload = serde_json::to_value(record).map_err(db)?;
    payload["management"] = serde_json::to_value(metadata).map_err(db)?;
    let payload = serde_json::to_string(&payload).map_err(db)?;
    client.execute(
        "INSERT INTO runbook.credential (id, org_id, secret_hash, payload, revoked_at, last_used_at) VALUES ($1,$2,$3,$4,$5,$6)",
        &[&record.id.as_str(), &record.org_id.as_str(), &&record.hash.as_bytes()[..], &payload, &record.revoked_at, &record.last_used_at],
    ).await.map_err(|e| if e.code() == Some(&SqlState::UNIQUE_VIOLATION) { CredentialError::Duplicate } else { db(e) })?;
    Ok(())
}
