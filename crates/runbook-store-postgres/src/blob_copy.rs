//! One-off copy of Postgres-held blobs to object storage, for an
//! install switching `RUNBOOK_BLOB_STORE` after the fact (follow-up
//! list). Each blob is copied under the same object path a fresh put
//! would use, verified by hash, and only then flipped to
//! location='object' with its chunks deleted — in one transaction, so
//! a crash leaves either the old readable row or the new one, and a
//! rerun copies whatever is left.
use crate::{
    DatabaseConfig,
    execution::{READ_COMMITTED, finish},
    pool::Pool,
    rows::{db, decode},
};
use runbook_core::{StoreError, blob::BlobKey, catalog::TenantScope, portable::sha256};
use runbook_store_objects::{ObjectStorage, ObjectStoreExt};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlobCopyStats {
    pub copied: usize,
    pub bytes: u64,
    /// Rows another copy (or a live put) flipped first.
    pub raced: usize,
}

pub struct BlobCopier {
    config: DatabaseConfig,
    pool: Rc<Pool>,
}

impl BlobCopier {
    pub fn new(config: DatabaseConfig) -> Self {
        Self {
            pool: Pool::new(config.clone()),
            config,
        }
    }

    /// Copy every `location = 'postgres'` blob, `batch` rows at a time.
    pub async fn copy_all(
        &self,
        storage: &ObjectStorage,
        batch: usize,
    ) -> Result<BlobCopyStats, StoreError> {
        let mut stats = BlobCopyStats::default();
        loop {
            let copied = self.copy_batch(storage, batch.max(1), &mut stats).await?;
            if copied == 0 {
                return Ok(stats);
            }
        }
    }

    async fn copy_batch(
        &self,
        storage: &ObjectStorage,
        batch: usize,
        stats: &mut BlobCopyStats,
    ) -> Result<usize, StoreError> {
        let conn = self.pool.get().await.map_err(crate::rows::connection)?;
        let rows = conn
            .client()
            .query(
                "SELECT org_id, project_id, key, sha256 FROM runbook.blob \
                 WHERE location = 'postgres' ORDER BY org_id, project_id, key LIMIT $1",
                &[&i64::try_from(batch)
                    .map_err(|_| StoreError::InvalidBatch("blob.copy_batch"))?],
            )
            .await
            .map_err(db)?;
        drop(conn);
        let mut flipped = 0;
        for row in &rows {
            let scope = TenantScope {
                org_id: parse(row.get(0))?,
                project_id: parse(row.get(1))?,
            };
            let key_text: &str = row.get(2);
            let stored_sha: Option<Vec<u8>> = row.get(3);
            let key: BlobKey = decode(key_text)?;
            let store = crate::PostgresExecutionStore::new(self.config.clone(), scope.clone());
            // Read through the public store: it reassembles chunks and
            // refuses a blob whose execution left the tenant.
            let bytes = match runbook_core::blob::BlobStore::get(&store, &key).await {
                Ok(bytes) => bytes,
                Err(StoreError::NotFound) => {
                    stats.raced += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let hash = sha256(&bytes);
            if let Some(stored) = &stored_sha
                && stored.as_slice() != hash
            {
                return Err(StoreError::Backend("blob.copy_hash_mismatch"));
            }
            let size = bytes.len() as u64;
            storage
                .store
                .put(&storage.path(&scope, &key, &hash), bytes.into())
                .await
                .map_err(|_| StoreError::Backend("object.put_failed"))?;
            // Flip and drop chunks only if the row is still Postgres-held.
            let mut conn = self.pool.get().await.map_err(crate::rows::connection)?;
            conn.begin(READ_COMMITTED).await.map_err(db)?;
            let outcome = async {
                let changed = conn
                    .client()
                    .execute(
                        "UPDATE runbook.blob SET location = 'object', sha256 = COALESCE(sha256, $4) \
                         WHERE org_id = $1 AND project_id = $2 AND key = $3 AND location = 'postgres'",
                        &[
                            &scope.org_id.as_str(),
                            &scope.project_id.as_str(),
                            &key_text,
                            &&hash[..],
                        ],
                    )
                    .await
                    .map_err(db)?;
                if changed == 1 {
                    conn.client()
                        .execute(
                            "DELETE FROM runbook.blob_chunk \
                             WHERE org_id = $1 AND project_id = $2 AND key = $3",
                            &[
                                &scope.org_id.as_str(),
                                &scope.project_id.as_str(),
                                &key_text,
                            ],
                        )
                        .await
                        .map_err(db)?;
                }
                Ok::<bool, StoreError>(changed == 1)
            }
            .await;
            let flipped_one = finish(&mut conn, outcome).await?;
            if flipped_one {
                stats.copied += 1;
                stats.bytes = stats.bytes.saturating_add(size);
                flipped += 1;
            } else {
                stats.raced += 1;
            }
        }
        Ok(flipped)
    }
}

fn parse<T: std::str::FromStr>(text: &str) -> Result<T, StoreError> {
    text.parse()
        .map_err(|_| StoreError::Backend("postgres.invalid_row"))
}
