//! R0-1: stored signing keys (`runbook.signing_keys`). The private key
//! arrives sealed by the host; this store never sees it in the clear.
use crate::{DatabaseConfig, rows::db};
use runbook_core::StoreError;

/// One row: a key's sealed private part and its public key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSigningKey {
    pub kid: String,
    /// `grant` or `user`.
    pub purpose: String,
    pub sealed_private: String,
    pub public: Vec<u8>,
    pub created_at_ms: i64,
    pub retired_at_ms: Option<i64>,
}

#[derive(Clone)]
pub struct PostgresSigningKeys {
    config: DatabaseConfig,
}
impl PostgresSigningKeys {
    pub fn new(config: DatabaseConfig) -> Self {
        Self { config }
    }

    /// Every active key and every key retired at or after `retired_since_ms`.
    pub async fn load(&self, retired_since_ms: i64) -> Result<Vec<StoredSigningKey>, StoreError> {
        let conn = crate::pool::Pool::new(self.config.clone())
            .get()
            .await
            .map_err(crate::rows::connection)?;
        let rows = conn
            .client()
            .query(
                "SELECT kid, purpose, sealed_private, public, created_at_ms, retired_at_ms \
                 FROM runbook.signing_keys \
                 WHERE retired_at_ms IS NULL OR retired_at_ms >= $1 \
                 ORDER BY purpose, created_at_ms, kid",
                &[&retired_since_ms],
            )
            .await
            .map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|row| StoredSigningKey {
                kid: row.get(0),
                purpose: row.get(1),
                sealed_private: row.get(2),
                public: row.get(3),
                created_at_ms: row.get(4),
                retired_at_ms: row.get(5),
            })
            .collect())
    }

    /// Insert a key as its purpose's active key unless one exists (another
    /// replica created it first). True when this key was stored.
    pub async fn insert_if_absent(&self, key: &StoredSigningKey) -> Result<bool, StoreError> {
        let conn = crate::pool::Pool::new(self.config.clone())
            .get()
            .await
            .map_err(crate::rows::connection)?;
        let inserted = conn
            .client()
            .execute(
                "INSERT INTO runbook.signing_keys \
                 (kid, purpose, sealed_private, public, created_at_ms, retired_at_ms) \
                 VALUES ($1, $2, $3, $4, $5, NULL) \
                 ON CONFLICT (purpose) WHERE retired_at_ms IS NULL DO NOTHING",
                &[
                    &key.kid,
                    &key.purpose,
                    &key.sealed_private,
                    &key.public,
                    &key.created_at_ms,
                ],
            )
            .await
            .map_err(db)?;
        Ok(inserted == 1)
    }

    /// Retire the purpose's active key at `now_ms` and make `key` active,
    /// in one transaction.
    pub async fn rotate(&self, key: &StoredSigningKey, now_ms: i64) -> Result<(), StoreError> {
        let mut conn = crate::pool::Pool::new(self.config.clone())
            .get()
            .await
            .map_err(crate::rows::connection)?;
        conn.begin(crate::execution::READ_COMMITTED)
            .await
            .map_err(db)?;
        let result = async {
            conn.client()
                .execute(
                    "UPDATE runbook.signing_keys SET retired_at_ms = $2 \
                     WHERE purpose = $1 AND retired_at_ms IS NULL",
                    &[&key.purpose, &now_ms],
                )
                .await?;
            conn.client()
                .execute(
                    "INSERT INTO runbook.signing_keys \
                     (kid, purpose, sealed_private, public, created_at_ms, retired_at_ms) \
                     VALUES ($1, $2, $3, $4, $5, NULL)",
                    &[
                        &key.kid,
                        &key.purpose,
                        &key.sealed_private,
                        &key.public,
                        &key.created_at_ms,
                    ],
                )
                .await
        }
        .await;
        match result {
            Ok(_) => conn.commit().await.map_err(db),
            Err(e) => {
                conn.rollback().await;
                Err(db(e))
            }
        }
    }
}
