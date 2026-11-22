//! Sealed Ed25519 keys on D1. Rotation is a single atomic batch.
use crate::sql::{Columns, D1, prepare};
use crate::store_error;
use runbook_core::StoreError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSigningKey {
    pub kid: String,
    pub purpose: String,
    pub sealed_private: String,
    pub public: Vec<u8>,
    pub created_at_ms: i64,
    pub retired_at_ms: Option<i64>,
}

#[derive(Clone)]
pub struct D1SigningKeys<D: D1> {
    db: D,
}

impl<D: D1> D1SigningKeys<D> {
    pub fn new(db: D) -> Self {
        Self { db }
    }

    pub async fn load(&self, retired_since_ms: i64) -> Result<Vec<StoredSigningKey>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT kid, purpose, sealed_private, public, created_at_ms, retired_at_ms \
                     FROM signing_keys WHERE retired_at_ms IS NULL OR retired_at_ms >= ? \
                     ORDER BY purpose, created_at_ms, kid",
                )
                .bind(retired_since_ms),
            )
            .await
            .map_err(store_error)?
            .into_iter()
            .map(|row| {
                Ok(StoredSigningKey {
                    kid: row.text(0)?.into(),
                    purpose: row.text(1)?.into(),
                    sealed_private: row.text(2)?.into(),
                    public: row.blob(3)?.to_vec(),
                    created_at_ms: row.integer(4)?,
                    retired_at_ms: row.optional_integer(5)?,
                })
            })
            .collect::<Result<_, _>>()
            .map_err(store_error)
    }

    pub async fn insert_if_absent(&self, key: &StoredSigningKey) -> Result<bool, StoreError> {
        self.db
            .run(insert(key))
            .await
            .map(|count| count == 1)
            .map_err(store_error)
    }

    pub async fn rotate(&self, key: &StoredSigningKey, now_ms: i64) -> Result<(), StoreError> {
        self.db
            .batch(vec![
                prepare(
                    "UPDATE signing_keys SET retired_at_ms = ? \
                     WHERE purpose = ? AND retired_at_ms IS NULL",
                )
                .bind(now_ms)
                .bind(&key.purpose),
                insert(key),
            ])
            .await
            .map(|_| ())
            .map_err(store_error)
    }
}

fn insert(key: &StoredSigningKey) -> crate::Statement {
    prepare(
        "INSERT INTO signing_keys (kid, purpose, sealed_private, public, created_at_ms) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT (purpose) WHERE retired_at_ms IS NULL DO NOTHING",
    )
    .bind(&key.kid)
    .bind(&key.purpose)
    .bind(&key.sealed_private)
    .bind(key.public.as_slice())
    .bind(key.created_at_ms)
}
