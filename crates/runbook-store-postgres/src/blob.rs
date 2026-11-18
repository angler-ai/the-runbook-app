//! B2a.3 BlobStore on PostgreSQL: one blob row plus ordinal bytea chunks.
//! Execution blobs belong to a run; shared blobs are addressed by the
//! SHA-256 of their bytes (D21: equal semantic hashes may have different
//! snapshot bytes, so content bytes, not the semantic hash, are the key).
//! Both namespaces are tenant-scoped and immutable per key.
use crate::{
    PostgresExecutionStore, catalog,
    execution::{READ_COMMITTED, SNAPSHOT_READ},
    rows::{db, decode, encode, uint},
};
use runbook_core::{
    StoreError,
    blob::{BLOB_CHUNK_BYTES, BlobKey, BlobPrefix, BlobStore},
    catalog::TenantScope,
};
use runbook_store_objects::{ObjectStoreExt, object_store};
use tokio_postgres::Client;

/// Whether retention deleted `id` in this tenant: its uploads stay until its
/// file expires (PRD §14), readable and deletable, never written again.
async fn retained(
    c: &Client,
    scope: &TenantScope,
    id: &runbook_model::ExecutionId,
) -> Result<bool, StoreError> {
    Ok(c.query_opt(
        "SELECT body FROM runbook.tombstone \
             WHERE execution_id = $1 AND org_id = $2 AND project_id = $3",
        &[
            &id.as_str(),
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
        ],
    )
    .await
    .map_err(db)?
    .map(|row| decode::<runbook_core::retention::Tombstone>(row.get(0)))
    .transpose()?
    .is_some_and(|t| t.reason == runbook_core::archives::RETENTION_REASON))
}

async fn check_blob(c: &Client, scope: &TenantScope, key: &BlobKey) -> Result<(), StoreError> {
    check_blob_for(c, scope, key, false).await
}

async fn check_blob_for(
    c: &Client,
    scope: &TenantScope,
    key: &BlobKey,
    write: bool,
) -> Result<(), StoreError> {
    match key {
        BlobKey::Execution { execution_id, name } => {
            if catalog::owner(c, execution_id).await?.as_ref() != Some(scope) {
                return Err(StoreError::NotFound);
            }
            if name.is_empty() || name.chars().any(char::is_control) {
                return Err(StoreError::InvalidBatch("blob.invalid_name"));
            }
        }
        // UP1: an upload belongs to its run's prefix (D49).
        BlobKey::Upload { execution_id, .. } => {
            if catalog::owner(c, execution_id).await?.as_ref() != Some(scope)
                && (write || !retained(c, scope, execution_id).await?)
            {
                return Err(StoreError::NotFound);
            }
        }
        // ARC1: a daily archive belongs to a runbook of this tenant (D53).
        BlobKey::Archive { runbook_id, .. } => {
            let owned: bool = c
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM runbook.runbook \
                     WHERE runbook_id = $1 AND org_id = $2 AND project_id = $3)",
                    &[
                        &runbook_id.as_str(),
                        &scope.org_id.as_str(),
                        &scope.project_id.as_str(),
                    ],
                )
                .await
                .map_err(db)?
                .get(0);
            if !owned {
                return Err(StoreError::NotFound);
            }
        }
        BlobKey::Shared { .. } => {}
    }
    Ok(())
}

/// Chunk ordinals and lengths of one blob, in order; None when absent.
async fn chunk_lengths(
    c: &Client,
    scope: &TenantScope,
    key_text: &str,
) -> Result<Option<Vec<(i32, u64)>>, StoreError> {
    let exists: bool = c
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM runbook.blob \
             WHERE org_id = $1 AND project_id = $2 AND key = $3)",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &key_text,
            ],
        )
        .await
        .map_err(db)?
        .get(0);
    if !exists {
        return Ok(None);
    }
    // octet_length reads a TOASTed value's size without fetching it.
    let rows = c
        .query(
            "SELECT ordinal, octet_length(bytes)::bigint FROM runbook.blob_chunk \
             WHERE org_id = $1 AND project_id = $2 AND key = $3 ORDER BY ordinal",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &key_text,
            ],
        )
        .await
        .map_err(db)?;
    rows.iter()
        .map(|row| Ok((row.get::<_, i32>(0), uint(row.get::<_, i64>(1))?)))
        .collect::<Result<Vec<_>, StoreError>>()
        .map(Some)
}

/// Insert a new blob row and its chunks; false when the key exists.
pub(crate) async fn insert_blob(
    c: &Client,
    scope: &TenantScope,
    key: &BlobKey,
    bytes: &[u8],
) -> Result<bool, StoreError> {
    let key_text = encode(key)?;
    let (execution_id, name, hash) = parts(key);
    let runbook_id = match key {
        BlobKey::Archive { runbook_id, .. } => Some(runbook_id.as_str()),
        _ => None,
    };
    let inserted = c
        .execute(
            "INSERT INTO runbook.blob \
             (org_id, project_id, key, execution_id, name, hash, runbook_id, created_at_us) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, CASE WHEN $7::text IS NULL THEN NULL \
               ELSE floor(extract(epoch FROM clock_timestamp()) * 1000000)::bigint END) \
             ON CONFLICT DO NOTHING",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &key_text,
                &execution_id,
                &name,
                &hash,
                &runbook_id,
            ],
        )
        .await
        .map_err(db)?;
    if inserted == 0 {
        return Ok(false);
    }
    for (ordinal, chunk) in bytes.chunks(BLOB_CHUNK_BYTES).enumerate() {
        let ordinal =
            i32::try_from(ordinal).map_err(|_| StoreError::InvalidBatch("blob.too_many_chunks"))?;
        c.execute(
            "INSERT INTO runbook.blob_chunk (org_id, project_id, key, ordinal, bytes) \
             VALUES ($1, $2, $3, $4, $5)",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &key_text,
                &ordinal,
                &chunk,
            ],
        )
        .await
        .map_err(db)?;
    }
    Ok(true)
}

/// Indexed columns beside the exact-JSON key, for per-run listing/deletion.
fn parts(key: &BlobKey) -> (Option<&str>, Option<&str>, Option<Vec<u8>>) {
    match key {
        BlobKey::Execution { execution_id, name } => {
            (Some(execution_id.as_str()), Some(name.as_str()), None)
        }
        BlobKey::Shared { hash } => (None, None, Some(hash.to_vec())),
        // UP1: under the run, no name; the key column carries the evd_ ID.
        BlobKey::Upload { execution_id, .. } => (Some(execution_id.as_str()), None, None),
        // ARC1: owned by its runbook (the `runbook_id` column, 0029).
        BlobKey::Archive { .. } => (None, None, None),
    }
}

pub(crate) async fn read_bytes(
    c: &Client,
    scope: &TenantScope,
    key_text: &str,
) -> Result<Option<Vec<u8>>, StoreError> {
    let exists: bool = c
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM runbook.blob \
             WHERE org_id = $1 AND project_id = $2 AND key = $3)",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &key_text,
            ],
        )
        .await
        .map_err(db)?
        .get(0);
    if !exists {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    for row in c
        .query(
            "SELECT bytes FROM runbook.blob_chunk \
             WHERE org_id = $1 AND project_id = $2 AND key = $3 ORDER BY ordinal",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &key_text,
            ],
        )
        .await
        .map_err(db)?
    {
        bytes.extend_from_slice(row.get::<_, &[u8]>(0));
    }
    Ok(Some(bytes))
}

pub(crate) async fn insert_blob_object(
    c: &Client,
    scope: &TenantScope,
    key: &BlobKey,
    size: u64,
    sha256: &[u8; 32],
) -> Result<bool, StoreError> {
    let key_text = encode(key)?;
    let (execution_id, name, hash) = parts(key);
    let runbook_id = match key {
        BlobKey::Archive { runbook_id, .. } => Some(runbook_id.as_str()),
        _ => None,
    };
    let size_i64 = crate::rows::int(size)?;
    let inserted = c
        .execute(
            "INSERT INTO runbook.blob \
             (org_id, project_id, key, execution_id, name, hash, runbook_id, created_at_us, size, sha256, location) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, CASE WHEN $7::text IS NULL THEN NULL \
               ELSE floor(extract(epoch FROM clock_timestamp()) * 1000000)::bigint END, \
               $8, $9, 'object') \
             ON CONFLICT DO NOTHING",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &key_text,
                &execution_id,
                &name,
                &hash,
                &runbook_id,
                &size_i64,
                &&sha256[..],
            ],
        )
        .await
        .map_err(db)?;
    Ok(inserted > 0)
}

impl BlobStore for PostgresExecutionStore {
    async fn put(&self, key: BlobKey, bytes: &[u8]) -> Result<(), StoreError> {
        if matches!(key, BlobKey::Archive { .. }) {
            let hook = self.archive_put_hook.borrow_mut().take();
            if let Some(hook) = hook {
                hook().await;
            }
        }
        let bytes = bytes.to_vec();
        let hash = runbook_core::portable::sha256(&bytes);
        if let BlobKey::Archive { digest, .. } = &key
            && hash != *digest
        {
            return Err(StoreError::InvalidBatch("blob.digest_mismatch"));
        }

        if let Some(storage) = &self.objects {
            self.transaction(SNAPSHOT_READ, async |c| {
                check_blob_for(c, &self.scope, &key, true).await
            })
            .await?;

            let path = storage.path(&self.scope, &key, &hash);
            storage
                .store
                .put(&path, bytes.clone().into())
                .await
                .map_err(|_| StoreError::Backend("object.put_failed"))?;

            let key_text = encode(&key)?;
            let outcome = self
                .transaction(READ_COMMITTED, async |c| {
                    if self.blob_put_faults.get() > 0 {
                        self.blob_put_faults.set(self.blob_put_faults.get() - 1);
                        return Err(StoreError::Backend("postgres.injected_fault"));
                    }
                    check_blob_for(c, &self.scope, &key, true).await?;
                    if insert_blob_object(c, &self.scope, &key, bytes.len() as u64, &hash).await? {
                        return Ok(None);
                    }
                    let row = c
                        .query_opt(
                            "SELECT location, sha256 FROM runbook.blob \
                             WHERE org_id = $1 AND project_id = $2 AND key = $3",
                            &[
                                &self.scope.org_id.as_str(),
                                &self.scope.project_id.as_str(),
                                &key_text,
                            ],
                        )
                        .await
                        .map_err(db)?;
                    let Some(row) = row else {
                        return Err(StoreError::Backend("postgres.blob_vanished"));
                    };
                    let location: String = row.get(0);
                    let row_sha256: Option<Vec<u8>> = row.get(1);
                    if let Some(existing_hash) = row_sha256 {
                        if existing_hash == hash {
                            Ok(None)
                        } else {
                            Ok(Some(StoreError::InvalidBatch("blob.immutable_key")))
                        }
                    } else if location == "postgres" {
                        let existing_bytes = read_bytes(c, &self.scope, &key_text)
                            .await?
                            .ok_or(StoreError::Backend("postgres.blob_vanished"))?;
                        if existing_bytes == bytes {
                            Ok(None)
                        } else {
                            Ok(Some(StoreError::InvalidBatch("blob.immutable_key")))
                        }
                    } else {
                        Ok(Some(StoreError::InvalidBatch("blob.immutable_key")))
                    }
                })
                .await;

            match outcome {
                Ok(None) => Ok(()),
                Ok(Some(err)) => {
                    let _ = storage.store.delete(&path).await;
                    Err(err)
                }
                Err(e) => Err(e),
            }
        } else {
            self.transaction(READ_COMMITTED, async |c| {
                check_blob_for(c, &self.scope, &key, true).await?;
                if insert_blob(c, &self.scope, &key, &bytes).await? {
                    return Ok(());
                }
                let existing = read_bytes(c, &self.scope, &encode(&key)?)
                    .await?
                    .ok_or(StoreError::Backend("postgres.blob_vanished"))?;
                if existing == bytes {
                    Ok(())
                } else {
                    Err(StoreError::InvalidBatch("blob.immutable_key"))
                }
            })
            .await
        }
    }

    async fn get(&self, key: &BlobKey) -> Result<Vec<u8>, StoreError> {
        let key_text = encode(key)?;
        let row_info = self
            .transaction(SNAPSHOT_READ, async |c| {
                check_blob(c, &self.scope, key).await?;
                let row = c
                    .query_opt(
                        "SELECT location, sha256 FROM runbook.blob \
                         WHERE org_id = $1 AND project_id = $2 AND key = $3",
                        &[
                            &self.scope.org_id.as_str(),
                            &self.scope.project_id.as_str(),
                            &key_text,
                        ],
                    )
                    .await
                    .map_err(db)?;
                let row = row.ok_or(StoreError::NotFound)?;
                let location: String = row.get(0);
                let sha256: Option<Vec<u8>> = row.get(1);
                if location == "postgres" {
                    let bytes = read_bytes(c, &self.scope, &key_text)
                        .await?
                        .ok_or(StoreError::NotFound)?;
                    Ok((location, sha256, Some(bytes)))
                } else {
                    Ok((location, sha256, None))
                }
            })
            .await?;

        if let Some(bytes) = row_info.2 {
            return Ok(bytes);
        }

        let sha256_vec = row_info
            .1
            .ok_or(StoreError::Backend("object.missing_sha256"))?;
        let sha256: [u8; 32] = sha256_vec
            .try_into()
            .map_err(|_| StoreError::Backend("object.invalid_sha256"))?;
        let storage = self
            .objects
            .as_ref()
            .ok_or(StoreError::Backend("object.not_configured"))?;
        let path = storage.path(&self.scope, key, &sha256);
        let result = storage.store.get(&path).await.map_err(|e| match e {
            object_store::Error::NotFound { .. } => StoreError::NotFound,
            _ => StoreError::Backend("object.get_failed"),
        })?;
        let bytes = result
            .bytes()
            .await
            .map_err(|_| StoreError::Backend("object.read_failed"))?;
        Ok(bytes.to_vec())
    }

    async fn get_range(
        &self,
        key: &BlobKey,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, StoreError> {
        let key_text = encode(key)?;
        let row_info = self
            .transaction(SNAPSHOT_READ, async |c| {
                check_blob(c, &self.scope, key).await?;
                let row = c
                    .query_opt(
                        "SELECT location, size, sha256 FROM runbook.blob \
                         WHERE org_id = $1 AND project_id = $2 AND key = $3",
                        &[
                            &self.scope.org_id.as_str(),
                            &self.scope.project_id.as_str(),
                            &key_text,
                        ],
                    )
                    .await
                    .map_err(db)?;
                let row = row.ok_or(StoreError::NotFound)?;
                let location: String = row.get(0);
                if location == "postgres" {
                    let chunks = chunk_lengths(c, &self.scope, &key_text)
                        .await?
                        .ok_or(StoreError::NotFound)?;
                    let size: u64 = chunks.iter().map(|(_, len)| len).sum();
                    if range.start > range.end || range.end > size {
                        return Err(StoreError::InvalidBatch("blob.invalid_range"));
                    }
                    let mut wanted = Vec::new();
                    let mut first_offset = None;
                    let mut offset = 0;
                    for (ordinal, len) in chunks {
                        if offset < range.end && offset + len > range.start {
                            first_offset.get_or_insert(offset);
                            wanted.push(ordinal);
                        }
                        offset += len;
                    }
                    let mut bytes = Vec::new();
                    if !wanted.is_empty() {
                        for row in c
                            .query(
                                "SELECT bytes FROM runbook.blob_chunk \
                                 WHERE org_id = $1 AND project_id = $2 AND key = $3 \
                                 AND ordinal = ANY($4) ORDER BY ordinal",
                                &[
                                    &self.scope.org_id.as_str(),
                                    &self.scope.project_id.as_str(),
                                    &key_text,
                                    &wanted,
                                ],
                            )
                            .await
                            .map_err(db)?
                        {
                            bytes.extend_from_slice(row.get::<_, &[u8]>(0));
                        }
                    }
                    let skip = (range.start - first_offset.unwrap_or(range.start)) as usize;
                    let take = (range.end - range.start) as usize;
                    Ok((
                        location,
                        None,
                        None,
                        Some(bytes.into_iter().skip(skip).take(take).collect()),
                    ))
                } else {
                    let size: i64 = row.get(1);
                    let sha256: Vec<u8> = row.get(2);
                    Ok((location, Some(size), Some(sha256), None))
                }
            })
            .await?;

        if let Some(bytes) = row_info.3 {
            return Ok(bytes);
        }

        let size = uint(
            row_info
                .1
                .ok_or(StoreError::Backend("object.missing_size"))?,
        )?;
        if range.start > range.end || range.end > size {
            return Err(StoreError::InvalidBatch("blob.invalid_range"));
        }
        if range.start == range.end {
            return Ok(Vec::new());
        }
        let sha256_vec = row_info
            .2
            .ok_or(StoreError::Backend("object.missing_sha256"))?;
        let sha256: [u8; 32] = sha256_vec
            .try_into()
            .map_err(|_| StoreError::Backend("object.invalid_sha256"))?;
        let storage = self
            .objects
            .as_ref()
            .ok_or(StoreError::Backend("object.not_configured"))?;
        let path = storage.path(&self.scope, key, &sha256);
        let bytes = storage
            .store
            .get_range(&path, range)
            .await
            .map_err(|e| match e {
                object_store::Error::NotFound { .. } => StoreError::NotFound,
                _ => StoreError::Backend("object.get_failed"),
            })?;
        Ok(bytes.to_vec())
    }

    async fn head(&self, key: &BlobKey) -> Result<runbook_core::blob::BlobHead, StoreError> {
        let key_text = encode(key)?;
        self.transaction(SNAPSHOT_READ, async |c| {
            check_blob(c, &self.scope, key).await?;
            let row = c
                .query_opt(
                    "SELECT location, size FROM runbook.blob \
                     WHERE org_id = $1 AND project_id = $2 AND key = $3",
                    &[
                        &self.scope.org_id.as_str(),
                        &self.scope.project_id.as_str(),
                        &key_text,
                    ],
                )
                .await
                .map_err(db)?;
            let row = row.ok_or(StoreError::NotFound)?;
            let location: String = row.get(0);
            if location == "object" {
                let size: i64 = row.get(1);
                Ok(runbook_core::blob::BlobHead { size: uint(size)? })
            } else {
                let chunks = chunk_lengths(c, &self.scope, &key_text)
                    .await?
                    .ok_or(StoreError::NotFound)?;
                Ok(runbook_core::blob::BlobHead {
                    size: chunks.iter().map(|(_, len)| len).sum(),
                })
            }
        })
        .await
    }

    async fn delete(&self, key: &BlobKey) -> Result<(), StoreError> {
        let key_text = encode(key)?;
        let row_opt = self
            .transaction(READ_COMMITTED, async |c| {
                check_blob(c, &self.scope, key).await?;
                let row = c
                    .query_opt(
                        "DELETE FROM runbook.blob \
                         WHERE org_id = $1 AND project_id = $2 AND key = $3 \
                         RETURNING location, sha256",
                        &[
                            &self.scope.org_id.as_str(),
                            &self.scope.project_id.as_str(),
                            &key_text,
                        ],
                    )
                    .await
                    .map_err(db)?;
                Ok(row)
            })
            .await?;

        let Some(row) = row_opt else {
            return Err(StoreError::NotFound);
        };

        let location: String = row.get(0);
        if location == "object" {
            let sha256: Option<Vec<u8>> = row.get(1);
            if let Some(sha256_vec) = sha256
                && let Ok(sha256) = <[u8; 32]>::try_from(sha256_vec)
                && let Some(storage) = &self.objects
            {
                let path = storage.path(&self.scope, key, &sha256);
                if let Err(e) = storage.store.delete(&path).await {
                    eprintln!("warning: failed to delete blob object {path}: {e}");
                }
            }
        }
        Ok(())
    }

    async fn list_prefix(&self, prefix: &BlobPrefix) -> Result<Vec<BlobKey>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            let rows = match prefix {
                BlobPrefix::Execution(id) => {
                    if catalog::owner(c, id).await?.as_ref() != Some(&self.scope) {
                        return Err(StoreError::NotFound);
                    }
                    c.query(
                        "SELECT key FROM runbook.blob \
                         WHERE org_id = $1 AND project_id = $2 AND execution_id = $3",
                        &[
                            &self.scope.org_id.as_str(),
                            &self.scope.project_id.as_str(),
                            &id.as_str(),
                        ],
                    )
                    .await
                    .map_err(db)?
                }
                BlobPrefix::Shared => c
                    .query(
                        "SELECT key FROM runbook.blob \
                         WHERE org_id = $1 AND project_id = $2 AND hash IS NOT NULL",
                        &[&self.scope.org_id.as_str(), &self.scope.project_id.as_str()],
                    )
                    .await
                    .map_err(db)?,
                // ARC2: the runbook's rows hold its files and its retained
                // uploads; keep this day's files.
                BlobPrefix::Archive { runbook_id, .. } => c
                    .query(
                        "SELECT key FROM runbook.blob \
                         WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3",
                        &[
                            &self.scope.org_id.as_str(),
                            &self.scope.project_id.as_str(),
                            &runbook_id.as_str(),
                        ],
                    )
                    .await
                    .map_err(db)?,
            };
            // The key column is the exact typed key (execution blobs and
            // uploads share a run's prefix).
            let mut keys = rows
                .iter()
                .map(|row| decode::<BlobKey>(row.get::<_, &str>(0)))
                .collect::<Result<Vec<_>, _>>()?;
            if let BlobPrefix::Archive { day, .. } = prefix {
                keys.retain(|k| matches!(k, BlobKey::Archive { day: d, .. } if d == day));
            }
            keys.sort();
            Ok(keys)
        })
        .await
    }
}
