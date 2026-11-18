//! UP1: `UploadStore` on the in-memory adapter. Each call is one staged
//! transaction on the shared database, stamped with the store clock.
use super::{Database, MemoryExecutionStore};
use crate::{
    StoreError,
    catalog::TenantScope,
    uploads::{EvidenceRecord, EvidenceState, UploadOutcome, UploadQuota, UploadStore},
};
use runbook_model::{EvidenceId, ExecutionId};

fn owned<'a>(db: &'a Database, scope: &TenantScope, id: &EvidenceId) -> Option<&'a EvidenceRecord> {
    db.evidence.get(id).filter(|r| &r.scope == scope)
}

/// Slots that count toward limits: pending and finalized, never rejected.
fn counted(record: &EvidenceRecord) -> bool {
    record.state != EvidenceState::Rejected
}

fn within_quota(db: &Database, record: &EvidenceRecord, quota: &UploadQuota) -> bool {
    let live = || db.evidence.values().filter(|r| counted(r));
    let image = record.declared_type.starts_with("image/");
    if let Some(cap) = quota.images_per_step_run
        && image
        && live()
            .filter(|r| {
                r.scope == record.scope
                    && r.step_run_id == record.step_run_id
                    && r.declared_type.starts_with("image/")
            })
            .count()
            >= cap as usize
    {
        return false;
    }
    if let Some(cap) = quota.files_per_execution
        && live()
            .filter(|r| r.scope == record.scope && r.execution_id == record.execution_id)
            .count()
            >= cap as usize
    {
        return false;
    }
    if let Some(cap) = quota.storage_bytes {
        let used: u64 = live()
            .chain(db.retained_evidence.values())
            .filter(|r| r.scope.org_id == record.scope.org_id)
            .map(EvidenceRecord::counted_bytes)
            .sum();
        if used.saturating_add(record.declared_size) > cap {
            return false;
        }
    }
    true
}

impl UploadStore for MemoryExecutionStore {
    async fn upload_time_ms(&self) -> Result<i64, StoreError> {
        Ok(self.clock.get())
    }
    async fn reserve_evidence(
        &self,
        scope: &TenantScope,
        record: EvidenceRecord,
        quota: &UploadQuota,
    ) -> Result<bool, StoreError> {
        self.transaction(|db| {
            if db.owners.get(&record.execution_id) != Some(scope) || &record.scope != scope {
                return Err(StoreError::NotFound);
            }
            if let Some(existing) = db.evidence.get(&record.id) {
                return if existing == &record {
                    Ok(false)
                } else {
                    Err(StoreError::InvalidBatch("upload.duplicate"))
                };
            }
            if record.state != EvidenceState::Pending {
                return Err(StoreError::InvalidBatch("upload.not_pending"));
            }
            if !within_quota(db, &record, quota) {
                return Err(StoreError::InvalidBatch("upload.limit_reached"));
            }
            db.evidence.insert(record.id.clone(), record);
            Ok(true)
        })
    }
    async fn get_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> Result<Option<EvidenceRecord>, StoreError> {
        Ok(owned(&self.database.borrow(), scope, id).cloned())
    }
    async fn finalize_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
        outcome: UploadOutcome,
        quota: &UploadQuota,
    ) -> Result<EvidenceRecord, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            let Some(current) = owned(db, scope, id) else {
                return Err(StoreError::NotFound);
            };
            if let (UploadOutcome::Verified(verified), Some(cap), EvidenceState::Pending) =
                (&outcome, quota.storage_bytes, current.state)
            {
                let others: u64 = db
                    .evidence
                    .values()
                    .chain(db.retained_evidence.values())
                    .filter(|r| r.scope.org_id == scope.org_id && r.id != *id && counted(r))
                    .map(EvidenceRecord::counted_bytes)
                    .sum();
                let mine = verified.size.saturating_add(verified.thumbnail_bytes);
                if others.saturating_add(mine) > cap {
                    return Err(StoreError::InvalidBatch("upload.limit_reached"));
                }
            }
            let record = db.evidence.get_mut(id).ok_or(StoreError::NotFound)?;
            if record.state != EvidenceState::Pending {
                return Err(StoreError::InvalidBatch("upload.not_pending"));
            }
            match outcome {
                UploadOutcome::Verified(verified) => {
                    record.state = EvidenceState::Finalized;
                    record.verified = Some(verified);
                }
                UploadOutcome::Rejected { .. } => record.state = EvidenceState::Rejected,
            }
            record.finalized_at_ms = Some(now);
            let record = record.clone();
            // ARC2: the manifest lists finalized evidence, so evidence
            // finalized on an archived run makes its file stale.
            if record.state == EvidenceState::Finalized {
                super::archives::mark_run_stale(db, &record.execution_id);
            }
            Ok(record)
        })
    }
    async fn list_evidence(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> Result<Vec<EvidenceRecord>, StoreError> {
        let mut out: Vec<_> = self
            .database
            .borrow()
            .evidence
            .values()
            .filter(|r| &r.scope == scope && &r.execution_id == execution_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| (a.created_at_ms, &a.id).cmp(&(b.created_at_ms, &b.id)));
        Ok(out)
    }
    async fn stale_evidence(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> Result<Vec<EvidenceRecord>, StoreError> {
        let mut out: Vec<_> = self
            .database
            .borrow()
            .evidence
            .values()
            .filter(|r| {
                &r.scope == scope
                    && r.state == EvidenceState::Pending
                    && r.created_at_ms < before_ms
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| (a.created_at_ms, &a.id).cmp(&(b.created_at_ms, &b.id)));
        out.truncate(limit);
        Ok(out)
    }
    async fn delete_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> Result<bool, StoreError> {
        self.transaction(|db| {
            if owned(db, scope, id).is_none() {
                return Ok(false);
            }
            Ok(db.evidence.remove(id).is_some())
        })
    }
}
