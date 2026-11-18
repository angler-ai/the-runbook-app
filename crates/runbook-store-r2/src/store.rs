//! `BlobStore` over two buckets, with the Postgres adapter's semantics:
//! - an execution blob or upload belongs to its run's tenant; an archive to
//!   its runbook's; a shared snapshot to the tenant prefix it sits under;
//! - every key is immutable: putting the same bytes again succeeds, other
//!   bytes are refused (`blob.immutable_key`); an archive is addressed by
//!   the SHA-256 of its bytes (`blob.digest_mismatch`);
//! - a foreign or absent key is `NotFound`; a bad range is
//!   `blob.invalid_range`.
use crate::{
    bucket::ObjectBucket,
    layout::{self, Bucket},
    owners::BlobOwners,
};
use runbook_core::{
    StoreError,
    blob::{BlobHead, BlobKey, BlobPrefix, BlobStore},
    catalog::TenantScope,
};
use std::ops::Range;

/// A tenant-bound blob store: `scope` comes from a verified credential.
#[derive(Clone)]
pub struct R2BlobStore<B: ObjectBucket, O: BlobOwners> {
    blobs: B,
    user_content: B,
    owners: O,
    scope: TenantScope,
}

impl<B: ObjectBucket, O: BlobOwners> R2BlobStore<B, O> {
    pub fn new(blobs: B, user_content: B, owners: O, scope: TenantScope) -> Self {
        Self {
            blobs,
            user_content,
            owners,
            scope,
        }
    }
    /// The same buckets seen by another tenant (every call still checks).
    pub fn scoped(&self, scope: TenantScope) -> Self {
        Self {
            scope,
            ..self.clone()
        }
    }
    pub fn scope(&self) -> &TenantScope {
        &self.scope
    }
    /// The blob bucket and the user-content bucket.
    pub fn buckets(&self) -> (&B, &B) {
        (&self.blobs, &self.user_content)
    }
    fn bucket(&self, which: Bucket) -> &B {
        match which {
            Bucket::Blobs => &self.blobs,
            Bucket::UserContent => &self.user_content,
        }
    }

    /// Delete every object one run owns (CF5 run deletion): its prefix in
    /// the blob bucket and, unless `keep_uploads` (retention keeps them
    /// with the archive), in the user-content bucket. No owner check: the
    /// caller's deletion has committed and removed the owner. Returns how
    /// many objects went.
    pub async fn purge_run(
        &self,
        id: &runbook_model::ExecutionId,
        keep_uploads: bool,
    ) -> Result<usize, StoreError> {
        let prefix = layout::run_prefix(&self.scope, id);
        let mut deleted = 0;
        let buckets: &[Bucket] = if keep_uploads {
            &[Bucket::Blobs]
        } else {
            &[Bucket::Blobs, Bucket::UserContent]
        };
        for which in buckets {
            let bucket = self.bucket(*which);
            loop {
                let page = bucket.list(&prefix, None).await?;
                if page.keys.is_empty() {
                    break;
                }
                for key in &page.keys {
                    bucket.delete(key).await?;
                    deleted += 1;
                }
            }
        }
        Ok(deleted)
    }

    /// CF8: every key of this project in both buckets (a portable export).
    pub async fn project_keys(&self) -> Result<Vec<BlobKey>, StoreError> {
        Ok(self.project_listing().await?.0)
    }
    /// [`Self::project_keys`] and how many list calls it took (CF12: each
    /// is a subrequest on Workers).
    pub async fn project_listing(&self) -> Result<(Vec<BlobKey>, u64), StoreError> {
        let prefix = layout::tenant(&self.scope);
        let (mut keys, mut calls) = self.keys(Bucket::Blobs, &prefix).await?;
        let (content, more) = self.keys(Bucket::UserContent, &prefix).await?;
        keys.extend(content);
        calls += more;
        keys.sort();
        Ok((keys, calls))
    }
    /// CF8: write one archived object before its owner rows exist (a
    /// portable import commits them after its objects). An existing object
    /// with the same bytes is accepted; other bytes are `import.id_conflict`.
    /// True when this call created the object.
    pub async fn import_put(&self, key: &BlobKey, bytes: &[u8]) -> Result<bool, StoreError> {
        if let BlobKey::Archive { digest, .. } = key
            && runbook_core::portable::sha256(bytes) != *digest
        {
            return Err(StoreError::InvalidBatch("blob.digest_mismatch"));
        }
        let (which, object) = layout::object(&self.scope, key);
        let bucket = self.bucket(which);
        if bucket.put_new(&object, bytes.to_vec()).await? {
            return Ok(true);
        }
        match bucket.get(&object).await? {
            Some(existing) if existing == bytes => Ok(false),
            _ => Err(StoreError::InvalidBatch("import.id_conflict")),
        }
    }
    /// CF8: one object's bytes with no owner check (a portable export
    /// reads every key the project's prefix holds).
    pub async fn export_get(&self, key: &BlobKey) -> Result<Vec<u8>, StoreError> {
        let (which, object) = layout::object(&self.scope, key);
        self.bucket(which)
            .get(&object)
            .await?
            .ok_or(StoreError::Backend("r2.blob_vanished"))
    }
    /// CF12b: store request replay response `name` (under the project's
    /// replay prefix, outside its blobs). The name is unique to its row; an
    /// existing object (a retried write) is kept.
    pub async fn reply_put(&self, name: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        let object = layout::replay_object(&self.scope, name);
        self.blobs.put_new(&object, bytes).await.map(|_| ())
    }
    /// CF12b: replay response `name`, if stored.
    pub async fn reply_get(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.blobs
            .get(&layout::replay_object(&self.scope, name))
            .await
    }
    /// CF12b: delete replay response `name` (its row has expired).
    pub async fn reply_delete(&self, name: &str) -> Result<(), StoreError> {
        self.blobs
            .delete(&layout::replay_object(&self.scope, name))
            .await
    }
    /// CF8: undo [`Self::import_put`] after a failed import.
    pub async fn import_delete(&self, key: &BlobKey) -> Result<(), StoreError> {
        let (which, object) = layout::object(&self.scope, key);
        self.bucket(which).delete(&object).await
    }

    /// Refuse a key this tenant may not touch (`write`: put, which a
    /// retained run's uploads never accept).
    async fn check(&self, key: &BlobKey, write: bool) -> Result<(), StoreError> {
        let owns = |owner: Option<TenantScope>| owner.as_ref() == Some(&self.scope);
        match key {
            BlobKey::Execution { execution_id, name } => {
                if !owns(self.owners.execution_owner(execution_id).await?) {
                    return Err(StoreError::NotFound);
                }
                if name.is_empty() || name.chars().any(char::is_control) {
                    return Err(StoreError::InvalidBatch("blob.invalid_name"));
                }
            }
            BlobKey::Upload { execution_id, .. } => {
                if !owns(self.owners.execution_owner(execution_id).await?)
                    && (write || !self.owners.retained(&self.scope, execution_id).await?)
                {
                    return Err(StoreError::NotFound);
                }
            }
            BlobKey::Archive { runbook_id, .. } => {
                if !owns(self.owners.runbook_owner(runbook_id).await?) {
                    return Err(StoreError::NotFound);
                }
            }
            // Tenant-scoped by its prefix.
            BlobKey::Shared { .. } => {}
        }
        Ok(())
    }

    async fn size(&self, key: &BlobKey) -> Result<u64, StoreError> {
        let (which, object) = layout::object(&self.scope, key);
        self.bucket(which)
            .head(&object)
            .await?
            .ok_or(StoreError::NotFound)
    }

    /// Every key under one object prefix of one bucket, across pages.
    async fn keys(&self, which: Bucket, prefix: &str) -> Result<(Vec<BlobKey>, u64), StoreError> {
        let mut keys = vec![];
        let mut cursor = None;
        let mut calls = 0;
        loop {
            calls += 1;
            let page = self.bucket(which).list(prefix, cursor).await?;
            keys.extend(
                page.keys
                    .iter()
                    .filter_map(|object| layout::parse(&self.scope, which, object)),
            );
            match page.cursor {
                Some(next) => cursor = Some(next),
                None => return Ok((keys, calls)),
            }
        }
    }
}

impl<B: ObjectBucket, O: BlobOwners> BlobStore for R2BlobStore<B, O> {
    async fn put(&self, key: BlobKey, bytes: &[u8]) -> Result<(), StoreError> {
        self.check(&key, true).await?;
        if let BlobKey::Archive { digest, .. } = &key
            && runbook_core::portable::sha256(bytes) != *digest
        {
            return Err(StoreError::InvalidBatch("blob.digest_mismatch"));
        }
        let (which, object) = layout::object(&self.scope, &key);
        let bucket = self.bucket(which);
        if bucket.put_new(&object, bytes.to_vec()).await? {
            return Ok(());
        }
        match bucket.get(&object).await? {
            Some(existing) if existing == bytes => Ok(()),
            Some(_) => Err(StoreError::InvalidBatch("blob.immutable_key")),
            // Deleted between the two calls: the caller may simply retry.
            None => Err(StoreError::Backend("r2.blob_vanished")),
        }
    }

    async fn get(&self, key: &BlobKey) -> Result<Vec<u8>, StoreError> {
        self.check(key, false).await?;
        let (which, object) = layout::object(&self.scope, key);
        self.bucket(which)
            .get(&object)
            .await?
            .ok_or(StoreError::NotFound)
    }

    async fn get_range(&self, key: &BlobKey, range: Range<u64>) -> Result<Vec<u8>, StoreError> {
        self.check(key, false).await?;
        let size = self.size(key).await?;
        if range.start > range.end || range.end > size {
            return Err(StoreError::InvalidBatch("blob.invalid_range"));
        }
        let (which, object) = layout::object(&self.scope, key);
        self.bucket(which)
            .get_range(&object, range.start, range.end - range.start)
            .await?
            .ok_or(StoreError::NotFound)
    }

    async fn head(&self, key: &BlobKey) -> Result<BlobHead, StoreError> {
        self.check(key, false).await?;
        Ok(BlobHead {
            size: self.size(key).await?,
        })
    }

    async fn delete(&self, key: &BlobKey) -> Result<(), StoreError> {
        self.check(key, false).await?;
        let (which, object) = layout::object(&self.scope, key);
        let bucket = self.bucket(which);
        if bucket.head(&object).await?.is_none() {
            return Err(StoreError::NotFound);
        }
        bucket.delete(&object).await
    }

    async fn list_prefix(&self, prefix: &BlobPrefix) -> Result<Vec<BlobKey>, StoreError> {
        let mut keys = match prefix {
            BlobPrefix::Execution(id) => {
                if self.owners.execution_owner(id).await?.as_ref() != Some(&self.scope) {
                    return Err(StoreError::NotFound);
                }
                // A run's blobs and its uploads share its prefix.
                let run = layout::run_prefix(&self.scope, id);
                let mut keys = self.keys(Bucket::Blobs, &format!("{run}blobs/")).await?.0;
                keys.extend(
                    self.keys(Bucket::UserContent, &format!("{run}uploads/"))
                        .await?
                        .0,
                );
                keys
            }
            BlobPrefix::Shared => {
                self.keys(Bucket::Blobs, &layout::shared_prefix(&self.scope))
                    .await?
                    .0
            }
            BlobPrefix::Archive { runbook_id, day } => {
                self.keys(
                    Bucket::Blobs,
                    &layout::archive_prefix(&self.scope, runbook_id, *day),
                )
                .await?
                .0
            }
        };
        keys.sort();
        Ok(keys)
    }
}
