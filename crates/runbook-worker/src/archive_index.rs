//! CF5: `ArchiveIndex` on the composed store. Each decision is one D1 batch
//! (`runbook_store_d1::archives`); the R2 objects it makes obsolete (older
//! versions of a day's file, a deleted file's versions, the uploads of
//! retained runs that follow their file) are deleted after the batch
//! commits. A version row is written before its object and marked stored
//! after it (`BlobStore::put` here), so a crash anywhere leaves rows the
//! sweep finds, never an object nothing points at.
use crate::compose::{CfStore, Objects};
use runbook_core::{
    StoreError,
    archives::{ArchiveIndex, ArchiveRecord, ArchivedRun},
    blob::{BlobKey, BlobStore},
    catalog::TenantScope,
    retention::Tombstone,
};
use runbook_model::{ExecutionId, RunbookId};
use runbook_store_d1::{D1, archives::D1Archives};
use runbook_store_r2::ObjectBucket;

impl<D: D1, O: Objects, B: ObjectBucket> CfStore<D, O, B> {
    fn archives(&self) -> D1Archives<D> {
        D1Archives::new(self.catalog().db().clone())
    }
    /// The bucket store bound to `scope` (archive keys check the runbook's
    /// tenant).
    fn blobs_of(
        &self,
        scope: &TenantScope,
    ) -> runbook_store_r2::R2BlobStore<B, crate::owners::D1Owners<D>> {
        self.blobs().scoped(scope.clone())
    }

    /// Delete the day's versions the index no longer needs: in R2, then
    /// their rows. Returns how many went.
    async fn sweep_day(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
    ) -> Result<usize, StoreError> {
        let archives = self.archives();
        let doomed = archives.doomed_versions(scope, runbook, day).await?;
        let blobs = self.blobs_of(scope);
        for digest in &doomed {
            let key = BlobKey::Archive {
                runbook_id: runbook.clone(),
                day,
                digest: *digest,
            };
            match blobs.delete(&key).await {
                Ok(()) | Err(StoreError::NotFound) => {}
                Err(error) => return Err(error),
            }
            archives.forget_version(scope, runbook, day, digest).await?;
        }
        Ok(doomed.len())
    }

    /// ARC2: retention may delete `id` at `revision` only behind a current
    /// file that lists it at that revision and is stored.
    pub(crate) async fn archive_current(
        &self,
        id: &ExecutionId,
        revision: u64,
    ) -> Result<bool, StoreError> {
        let Some(file) = self
            .archives()
            .current_file_of(self.scope(), id, revision)
            .await?
        else {
            return Ok(false);
        };
        match self
            .blobs()
            .head(&BlobKey::Archive {
                runbook_id: file.runbook_id,
                day: file.day,
                digest: file.digest,
            })
            .await
        {
            Ok(_) => Ok(true),
            Err(StoreError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// A put of an archive version: its row first (pending), then R2, then
    /// the row marked stored.
    pub(crate) async fn put_archive_version(
        &self,
        key: BlobKey,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let BlobKey::Archive {
            runbook_id,
            day,
            digest,
        } = &key
        else {
            return self.blobs().put(key, bytes).await;
        };
        if runbook_core::portable::sha256(bytes) != *digest {
            return Err(StoreError::InvalidBatch("blob.digest_mismatch"));
        }
        let (runbook_id, day, digest) = (runbook_id.clone(), *day, *digest);
        // The tenant must own the runbook before a row is written.
        if self.catalog().read_runbook(&runbook_id).await?.is_none() {
            return Err(StoreError::NotFound);
        }
        let archives = self.archives();
        archives
            .begin_version(self.scope(), &runbook_id, day, &digest)
            .await?;
        self.blobs().put(key, bytes).await?;
        archives
            .version_stored(self.scope(), &runbook_id, day, &digest)
            .await
    }

    /// A delete of an archive version: R2, then its row.
    pub(crate) async fn delete_archive_version(&self, key: &BlobKey) -> Result<(), StoreError> {
        self.blobs().delete(key).await?;
        if let BlobKey::Archive {
            runbook_id,
            day,
            digest,
        } = key
        {
            self.archives()
                .forget_version(self.scope(), runbook_id, *day, digest)
                .await?;
        }
        Ok(())
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> ArchiveIndex for CfStore<D, O, B> {
    async fn record_archive(
        &self,
        scope: &TenantScope,
        record: ArchiveRecord,
        runs: Vec<ArchivedRun>,
        expected: Option<[u8; 32]>,
    ) -> Result<(), StoreError> {
        let (runbook, day) = (record.runbook_id.clone(), record.day);
        self.archives()
            .record_archive(scope, record, runs, expected)
            .await?;
        // The day's older versions go once the index names the new one; a
        // failure leaves rows the leftover sweep finds.
        let _ = self.sweep_day(scope, &runbook, day).await;
        Ok(())
    }
    async fn mark_archive_stale(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<bool, StoreError> {
        self.archives()
            .mark_archive_stale(scope, runbook_id, day)
            .await
    }
    async fn get_archive(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<Option<ArchiveRecord>, StoreError> {
        self.archives().get_archive(scope, runbook_id, day).await
    }
    async fn list_archives(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        from_day: Option<i64>,
        to_day: Option<i64>,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.archives()
            .list_archives(scope, runbook_id, from_day, to_day)
            .await
    }
    async fn archived_run(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> Result<Option<ArchivedRun>, StoreError> {
        self.archives().archived_run(scope, execution_id).await
    }
    async fn stale_archives(
        &self,
        scope: &TenantScope,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.archives().stale_archives(scope, now_ms, limit).await
    }
    async fn delete_archive(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
        expected: [u8; 32],
    ) -> Result<bool, StoreError> {
        let Some(deleted) = self
            .archives()
            .delete_archive(scope, runbook_id, day, expected)
            .await?
        else {
            return Ok(false);
        };
        let blobs = self.blobs_of(scope);
        let archives = self.archives();
        for digest in &deleted.versions {
            match blobs
                .delete(&BlobKey::Archive {
                    runbook_id: runbook_id.clone(),
                    day,
                    digest: *digest,
                })
                .await
            {
                Ok(()) | Err(StoreError::NotFound) => {}
                Err(error) => return Err(error),
            }
            archives
                .forget_version(scope, runbook_id, day, digest)
                .await?;
        }
        // The retained runs' uploads follow the file (PRD §14).
        for id in &deleted.retained_runs {
            blobs.purge_run(id, false).await?;
        }
        Ok(true)
    }
    async fn file_members(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.archives().file_members(scope, runbook_id, day).await
    }
    async fn sweep_leftover_versions(
        &self,
        scope: &TenantScope,
        limit: usize,
    ) -> Result<usize, StoreError> {
        let mut swept = 0;
        for (runbook, day) in self.archives().leftover_days(scope, limit).await? {
            swept += self.sweep_day(scope, &runbook, day).await?;
        }
        Ok(swept)
    }
    async fn unarchived_days(
        &self,
        scope: &TenantScope,
        before_day: i64,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<(RunbookId, i64)>, StoreError> {
        self.archives()
            .unarchived_days(scope, before_day, now_ms, limit)
            .await
    }
    async fn archived_live_runs(
        &self,
        scope: &TenantScope,
        completed_before_ms: i64,
        limit: usize,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.archives()
            .archived_live_runs(scope, completed_before_ms, limit)
            .await
    }
    async fn expired_archives(
        &self,
        scope: &TenantScope,
        before_day: i64,
        limit: usize,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.archives()
            .expired_archives(scope, before_day, limit)
            .await
    }
    async fn purge_summaries(
        &self,
        scope: &TenantScope,
        completed_before_ms: i64,
        limit: usize,
    ) -> Result<usize, StoreError> {
        self.archives()
            .purge_summaries(scope, completed_before_ms, limit)
            .await
    }
    async fn archive_failed(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
        now_ms: i64,
    ) -> Result<u32, StoreError> {
        self.archives()
            .archive_failed(scope, runbook_id, day, now_ms)
            .await
    }
    async fn sweep_archive_versions(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<(), StoreError> {
        self.sweep_day(scope, runbook_id, day).await.map(|_| ())
    }
    async fn purge_retained_run(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
        reason: &str,
    ) -> Result<Option<Tombstone>, StoreError> {
        let tombstone = self
            .archives()
            .purge_retained_run(scope, execution_id, reason)
            .await?;
        if tombstone.is_some() {
            self.blobs_of(scope).purge_run(execution_id, false).await?;
        }
        Ok(tombstone)
    }
}
