//! CF5: evidence upload slots (`UploadStore`, UP1/D49) on D1. The bytes are
//! R2's (`BlobKey::Upload`, CF4); the slot rows are `0028_uploads`.
//!
//! Postgres serializes reserve and finalize per organization with an
//! advisory lock and checks the quotas under it. D1 has no locks, so each
//! write is one guarded batch: the quota conditions are SQL over the
//! current rows, evaluated by the batch itself, so two racing reserves can
//! never both pass a cap one of them fills (D1 runs a batch as one
//! serialized transaction). A slot's run must be owned by the caller's
//! tenant; its rows cascade with the owner row, so a deleted run's slots go
//! with it.
use crate::{
    catalog::{count, decode, err},
    sql::{D1, D1Error, Statement, guard, json, prepare, uint},
};
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    uploads::{EvidenceRecord, EvidenceState, UploadOutcome, UploadQuota, UploadStore},
};
use runbook_model::{EvidenceId, ExecutionId};

/// What a slot counts toward its organization's storage, in SQL over an
/// `evidence` row (`EvidenceRecord::counted_bytes`): the stored file and
/// its thumbnail once finalized, the declared size before, nothing once
/// rejected (callers filter rejected rows out).
pub(crate) const COUNTED_BYTES: &str = "CASE WHEN state = 'finalized' THEN \
     COALESCE(json_extract(body, '$.verified.size'), declared_size) \
     + COALESCE(json_extract(body, '$.verified.thumbnail_bytes'), 0) \
     ELSE declared_size END";

/// The organization's counted bytes, except one slot: live slots plus the
/// retained evidence of runs retention deleted (ARC2). Binds `(org,
/// except, org, except)`.
fn storage_used_sql() -> String {
    format!(
        "(COALESCE((SELECT sum({COUNTED_BYTES}) FROM evidence \
           WHERE org_id = ? AND state <> 'rejected' AND evidence_id <> ?), 0) \
         + COALESCE((SELECT sum(counted_bytes) FROM retained_evidence \
           WHERE org_id = ? AND evidence_id <> ?), 0))"
    )
}

/// Mark the archive file listing `execution_id` stale (ARC2: the manifest
/// lists finalized evidence). Found through the file's membership, which
/// outlives the summary row.
pub(crate) fn mark_run_stale(execution_id: &ExecutionId) -> Statement {
    prepare(
        "UPDATE archive SET stale = 1, stale_marks = stale_marks + 1 \
         WHERE (org_id, project_id, runbook_id, day) IN \
         (SELECT org_id, project_id, runbook_id, day FROM archive_member WHERE execution_id = ?)",
    )
    .bind(execution_id.as_str())
}

/// Evidence slots on D1. Every call is scoped by its `scope` argument.
#[derive(Clone)]
pub struct D1Uploads<D: D1> {
    db: D,
}
impl<D: D1> D1Uploads<D> {
    pub fn new(db: D) -> Self {
        Self { db }
    }

    async fn read(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> Result<Option<EvidenceRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM evidence \
                     WHERE evidence_id = ? AND org_id = ? AND project_id = ?",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| decode(row, 0))
            .transpose()
    }

    /// The quota guards of a new slot, evaluated by its insert's batch.
    fn quota_guards(
        record: &EvidenceRecord,
        quota: &UploadQuota,
    ) -> Result<Vec<Statement>, StoreError> {
        let (org, project) = (
            record.scope.org_id.as_str(),
            record.scope.project_id.as_str(),
        );
        let mut guards = vec![];
        if let Some(cap) = quota.images_per_step_run
            && record.declared_type.starts_with("image/")
        {
            guards.push(
                guard(
                    "upload.limit_reached",
                    "(SELECT count(*) FROM evidence WHERE org_id = ? AND project_id = ? \
                     AND step_run_id = ? AND is_image = 1 AND state <> 'rejected') < ?",
                )
                .bind(org)
                .bind(project)
                .bind(record.step_run_id.as_str())
                .bind(i64::from(cap)),
            );
        }
        if let Some(cap) = quota.files_per_execution {
            guards.push(
                guard(
                    "upload.limit_reached",
                    "(SELECT count(*) FROM evidence WHERE org_id = ? AND project_id = ? \
                     AND execution_id = ? AND state <> 'rejected') < ?",
                )
                .bind(org)
                .bind(project)
                .bind(record.execution_id.as_str())
                .bind(i64::from(cap)),
            );
        }
        if let Some(cap) = quota.storage_bytes {
            guards.push(
                guard(
                    "upload.limit_reached",
                    &format!("{} + ? <= ?", storage_used_sql()),
                )
                .bind(org)
                .bind("")
                .bind(org)
                .bind("")
                .bind(uint(record.declared_size).map_err(err)?)
                .bind(uint(cap).map_err(err)?),
            );
        }
        Ok(guards)
    }
}

impl<D: D1> UploadStore for D1Uploads<D> {
    async fn upload_time_ms(&self) -> Result<i64, StoreError> {
        Ok(self.db.now_ms())
    }

    async fn reserve_evidence(
        &self,
        scope: &TenantScope,
        record: EvidenceRecord,
        quota: &UploadQuota,
    ) -> Result<bool, StoreError> {
        if &record.scope != scope
            || crate::catalog::execution_owner(&self.db, &record.execution_id)
                .await?
                .as_ref()
                != Some(scope)
        {
            return Err(StoreError::NotFound);
        }
        // Two passes at most: a racing insert of the same ID is read back
        // and compared like any retry.
        for _ in 0..2 {
            let existing = self
                .db
                .all(
                    prepare("SELECT body FROM evidence WHERE evidence_id = ?")
                        .bind(record.id.as_str()),
                )
                .await
                .map_err(err)?;
            if let Some(row) = existing.first() {
                let existing: EvidenceRecord = decode(row, 0)?;
                return if existing == record {
                    Ok(false)
                } else {
                    Err(StoreError::InvalidBatch("upload.duplicate"))
                };
            }
            if record.state != EvidenceState::Pending {
                return Err(StoreError::InvalidBatch("upload.not_pending"));
            }
            let mut statements = vec![
                guard(
                    "not_found",
                    "EXISTS (SELECT 1 FROM execution_owner WHERE execution_id = ? \
                     AND org_id = ? AND project_id = ?)",
                )
                .bind(record.execution_id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
                guard(
                    "duplicate",
                    "NOT EXISTS (SELECT 1 FROM evidence WHERE evidence_id = ?)",
                )
                .bind(record.id.as_str()),
            ];
            statements.extend(Self::quota_guards(&record, quota)?);
            statements.push(
                prepare(
                    "INSERT INTO evidence (evidence_id, org_id, project_id, execution_id, \
                     step_run_id, state, is_image, declared_size, created_at_ms, body) \
                     VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?)",
                )
                .bind(record.id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(record.execution_id.as_str())
                .bind(record.step_run_id.as_str())
                .bind(i64::from(record.declared_type.starts_with("image/")))
                .bind(uint(record.declared_size).map_err(err)?)
                .bind(record.created_at_ms)
                .bind(json(&record).map_err(err)?),
            );
            match self.db.batch(statements).await {
                Ok(_) => return Ok(true),
                Err(D1Error::Guard(code)) if code == "duplicate" => {}
                Err(error) => return Err(err(error)),
            }
        }
        Err(StoreError::InvalidBatch("upload.duplicate"))
    }

    async fn get_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> Result<Option<EvidenceRecord>, StoreError> {
        self.read(scope, id).await
    }

    async fn finalize_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
        outcome: UploadOutcome,
        quota: &UploadQuota,
    ) -> Result<EvidenceRecord, StoreError> {
        let mut record = self.read(scope, id).await?.ok_or(StoreError::NotFound)?;
        if record.state != EvidenceState::Pending {
            return Err(StoreError::InvalidBatch("upload.not_pending"));
        }
        let mut statements = vec![
            guard(
                "upload.not_pending",
                "EXISTS (SELECT 1 FROM evidence WHERE evidence_id = ? AND org_id = ? \
                 AND project_id = ? AND state = 'pending')",
            )
            .bind(id.as_str())
            .bind(scope.org_id.as_str())
            .bind(scope.project_id.as_str()),
        ];
        if let (UploadOutcome::Verified(verified), Some(cap)) = (&outcome, quota.storage_bytes) {
            statements.push(
                guard(
                    "upload.limit_reached",
                    &format!("{} + ? <= ?", storage_used_sql()),
                )
                .bind(scope.org_id.as_str())
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(id.as_str())
                .bind(uint(verified.size.saturating_add(verified.thumbnail_bytes)).map_err(err)?)
                .bind(uint(cap).map_err(err)?),
            );
        }
        match outcome {
            UploadOutcome::Verified(verified) => {
                record.state = EvidenceState::Finalized;
                record.verified = Some(verified);
            }
            UploadOutcome::Rejected { .. } => record.state = EvidenceState::Rejected,
        }
        record.finalized_at_ms = Some(self.db.now_ms());
        let state = match record.state {
            EvidenceState::Finalized => "finalized",
            EvidenceState::Rejected => "rejected",
            EvidenceState::Pending => "pending",
        };
        statements.push(
            prepare("UPDATE evidence SET state = ?, body = ? WHERE evidence_id = ?")
                .bind(state)
                .bind(json(&record).map_err(err)?)
                .bind(id.as_str()),
        );
        if record.state == EvidenceState::Finalized {
            statements.push(mark_run_stale(&record.execution_id));
        }
        self.db.batch(statements).await.map_err(err)?;
        Ok(record)
    }

    async fn list_evidence(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> Result<Vec<EvidenceRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM evidence WHERE org_id = ? AND project_id = ? \
                     AND execution_id = ? ORDER BY created_at_ms, evidence_id",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(execution_id.as_str()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| decode(row, 0))
            .collect()
    }

    async fn stale_evidence(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> Result<Vec<EvidenceRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM evidence WHERE org_id = ? AND project_id = ? \
                     AND state = 'pending' AND created_at_ms < ? \
                     ORDER BY created_at_ms, evidence_id LIMIT ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(before_ms)
                .bind(count(limit)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| decode(row, 0))
            .collect()
    }

    async fn delete_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> Result<bool, StoreError> {
        let rows = self
            .db
            .all(
                prepare(
                    "DELETE FROM evidence WHERE evidence_id = ? AND org_id = ? \
                     AND project_id = ? RETURNING evidence_id",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?;
        Ok(!rows.is_empty())
    }
}
