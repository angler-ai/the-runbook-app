//! AUTH2 storage for the RFC 8628 device grant (D47). Each poll and decision
//! locks the code's row, applies the shared `runbook_core::device_codes`
//! rules and writes the record back in one transaction.
use super::*;
use crate::execution::READ_COMMITTED;
use runbook_core::device_codes::{
    DeviceCodeRecord, DeviceCodeState, DeviceCodeStore, DeviceDecision, DevicePoll, apply_decision,
    apply_poll,
};

/// Expired codes are purged this long after expiry, on each insert.
const PURGE_AFTER_MS: i64 = 86_400_000;

fn org_of(record: &DeviceCodeRecord) -> Option<&str> {
    match &record.state {
        DeviceCodeState::Approved(grant) => Some(grant.org_id.as_str()),
        _ => None,
    }
}

async fn update(
    client: &tokio_postgres::Client,
    record: &DeviceCodeRecord,
) -> Result<(), CredentialError> {
    let payload = serde_json::to_string(record).map_err(db)?;
    client
        .execute(
            "UPDATE runbook.device_code SET payload = $2, org_id = COALESCE(org_id, $3) \
             WHERE device_code_hash = $1",
            &[
                &&record.device_code_hash.as_bytes()[..],
                &payload,
                &org_of(record),
            ],
        )
        .await
        .map_err(db)?;
    Ok(())
}

/// Lock one row by a hash column, change it with `f`, write it back.
async fn locked<T>(
    store: &PostgresCredentials,
    column: &str,
    hash: &SecretHash,
    f: impl FnOnce(Option<&mut DeviceCodeRecord>) -> T,
) -> Result<T, CredentialError> {
    let mut conn = store.pool.get().await.map_err(db)?;
    conn.begin(READ_COMMITTED).await.map_err(db)?;
    let result = async {
        let row = conn
            .client()
            .query_opt(
                &format!("SELECT payload FROM runbook.device_code WHERE {column} = $1 FOR UPDATE"),
                &[&&hash.as_bytes()[..]],
            )
            .await
            .map_err(db)?;
        let mut record: Option<DeviceCodeRecord> = row
            .map(|r| serde_json::from_str(r.get(0)).map_err(db))
            .transpose()?;
        let before = record.clone();
        let out = f(record.as_mut());
        if let Some(record) = &record
            && Some(record) != before.as_ref()
        {
            update(conn.client(), record).await?;
        }
        Ok(out)
    }
    .await;
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

impl PostgresCredentials {
    /// A device code's record by the CLI's code (for the minted token's
    /// name). Grants nothing.
    pub async fn device_code_by_device_hash(
        &self,
        device_code_hash: &SecretHash,
    ) -> Result<Option<DeviceCodeRecord>, CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        conn.client()
            .query_opt(
                "SELECT payload FROM runbook.device_code WHERE device_code_hash = $1",
                &[&&device_code_hash.as_bytes()[..]],
            )
            .await
            .map_err(db)?
            .map(|r| serde_json::from_str(r.get(0)).map_err(db))
            .transpose()
    }
}

impl DeviceCodeStore for PostgresCredentials {
    async fn insert_device_code(&self, record: DeviceCodeRecord) -> Result<(), CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        conn.client()
            .execute(
                "DELETE FROM runbook.device_code WHERE expires_at < $1",
                &[&record.created_at_ms.saturating_sub(PURGE_AFTER_MS)],
            )
            .await
            .map_err(db)?;
        let payload = serde_json::to_string(&record).map_err(db)?;
        match conn
            .client()
            .execute(
                "INSERT INTO runbook.device_code \
                 (device_code_hash, user_code_hash, org_id, expires_at, payload) \
                 VALUES ($1, $2, $3, $4, $5)",
                &[
                    &&record.device_code_hash.as_bytes()[..],
                    &&record.user_code_hash.as_bytes()[..],
                    &org_of(&record),
                    &record.expires_at_ms,
                    &payload,
                ],
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.code() == Some(&SqlState::UNIQUE_VIOLATION) => {
                Err(CredentialError::Duplicate)
            }
            Err(e) => Err(db(e)),
        }
    }
    async fn device_code_by_user_code(
        &self,
        user_code_hash: &SecretHash,
    ) -> Result<Option<DeviceCodeRecord>, CredentialError> {
        let conn = self.pool.get().await.map_err(db)?;
        conn.client()
            .query_opt(
                "SELECT payload FROM runbook.device_code WHERE user_code_hash = $1",
                &[&&user_code_hash.as_bytes()[..]],
            )
            .await
            .map_err(db)?
            .map(|r| serde_json::from_str(r.get(0)).map_err(db))
            .transpose()
    }
    async fn poll_device_code(
        &self,
        device_code_hash: &SecretHash,
        token_id: &runbook_model::TokenId,
        now_ms: i64,
    ) -> Result<DevicePoll, CredentialError> {
        locked(self, "device_code_hash", device_code_hash, |record| {
            record.map_or(DevicePoll::Expired, |r| apply_poll(r, token_id, now_ms))
        })
        .await
    }
    async fn decide_device_code(
        &self,
        org: &OrganizationId,
        user_code_hash: &SecretHash,
        decision: DeviceDecision,
        now_ms: i64,
    ) -> Result<bool, CredentialError> {
        locked(self, "user_code_hash", user_code_hash, |record| {
            record.is_some_and(|r| apply_decision(r, org, decision, now_ms))
        })
        .await
    }
}
