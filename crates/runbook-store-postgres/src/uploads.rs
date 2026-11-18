//! UP1: `UploadStore` on PostgreSQL (migration 0028). Each call is one
//! READ COMMITTED transaction filtered by organization and project. Limits
//! are counted under a transaction-scoped advisory lock on the organization
//! (storage is per organization), so concurrent reservations cannot both take
//! the last slot. The bytes are ordinary run blobs (`BlobKey::Upload`).
use crate::{
    catalog,
    execution::{PostgresExecutionStore, READ_COMMITTED, SNAPSHOT_READ},
    rows::{db, decode, encode, int, label, uint},
};
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    uploads::{EvidenceRecord, EvidenceState, UploadOutcome, UploadQuota, UploadStore},
};
use runbook_model::{EvidenceId, ExecutionId};
use tokio_postgres::Client;

const NOW_MS: &str = "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint";

async fn now_ms(c: &Client) -> Result<i64, StoreError> {
    c.query_one(NOW_MS, &[]).await.map(|r| r.get(0)).map_err(db)
}

async fn read(
    c: &Client,
    scope: &TenantScope,
    id: &EvidenceId,
    lock: bool,
) -> Result<Option<EvidenceRecord>, StoreError> {
    let sql = if lock {
        "SELECT body FROM runbook.evidence \
         WHERE evidence_id = $1 AND org_id = $2 AND project_id = $3 FOR UPDATE"
    } else {
        "SELECT body FROM runbook.evidence \
         WHERE evidence_id = $1 AND org_id = $2 AND project_id = $3"
    };
    c.query_opt(
        sql,
        &[
            &id.as_str(),
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
        ],
    )
    .await
    .map_err(db)?
    .map(|row| decode(row.get::<_, &str>(0)))
    .transpose()
}

async fn count(c: &Client, sql: &str, args: &[&str]) -> Result<u64, StoreError> {
    let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = args
        .iter()
        .map(|a| a as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();
    uint(
        c.query_one(sql, &params)
            .await
            .map_err(db)?
            .get::<_, i64>(0),
    )
}

async fn within_quota(
    c: &Client,
    record: &EvidenceRecord,
    quota: &UploadQuota,
) -> Result<bool, StoreError> {
    let (org, project) = (
        record.scope.org_id.as_str(),
        record.scope.project_id.as_str(),
    );
    if let Some(cap) = quota.images_per_step_run
        && record.declared_type.starts_with("image/")
        && count(
            c,
            "SELECT count(*) FROM runbook.evidence WHERE org_id = $1 AND project_id = $2 \
             AND step_run_id = $3 AND is_image AND state <> 'rejected'",
            &[org, project, record.step_run_id.as_str()],
        )
        .await?
            >= u64::from(cap)
    {
        return Ok(false);
    }
    if let Some(cap) = quota.files_per_execution
        && count(
            c,
            "SELECT count(*) FROM runbook.evidence WHERE org_id = $1 AND project_id = $2 \
             AND execution_id = $3 AND state <> 'rejected'",
            &[org, project, record.execution_id.as_str()],
        )
        .await?
            >= u64::from(cap)
    {
        return Ok(false);
    }
    if let Some(cap) = quota.storage_bytes
        && storage_used(c, org, None)
            .await?
            .saturating_add(record.declared_size)
            > cap
    {
        return Ok(false);
    }
    Ok(true)
}

/// What one live slot counts (`EvidenceRecord::counted_bytes`: stored bytes
/// once finalized, the declared size before), read from the exact body.
pub(crate) const COUNTED_BYTES: &str = "CASE WHEN state = 'finalized' THEN \
       COALESCE((body::jsonb #>> '{verified,size}')::bigint, declared_size) \
       + COALESCE((body::jsonb #>> '{verified,thumbnail_bytes}')::bigint, 0) \
     ELSE declared_size END";

/// Bytes the organization's slots count, leaving out `except`: live slots,
/// plus the evidence of runs retention deleted, which follows the archive
/// and counts until the file expires (ARC2, PRD §14).
async fn storage_used(c: &Client, org: &str, except: Option<&str>) -> Result<u64, StoreError> {
    count(
        c,
        &format!(
            "SELECT (COALESCE((SELECT sum({COUNTED_BYTES}) FROM runbook.evidence \
               WHERE org_id = $1 AND state <> 'rejected' AND evidence_id <> $2), 0) \
             + COALESCE((SELECT sum(counted_bytes) FROM runbook.retained_evidence \
               WHERE org_id = $1 AND evidence_id <> $2), 0))::bigint"
        ),
        &[org, except.unwrap_or("")],
    )
    .await
}

/// The per-organization lock limits are counted under.
async fn lock_org(c: &Client, scope: &TenantScope) -> Result<(), StoreError> {
    c.query_one(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        &[&format!("uploads:{}", scope.org_id)],
    )
    .await
    .map_err(db)?;
    Ok(())
}

impl UploadStore for PostgresExecutionStore {
    async fn upload_time_ms(&self) -> Result<i64, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| now_ms(c).await)
            .await
    }

    async fn reserve_evidence(
        &self,
        scope: &TenantScope,
        record: EvidenceRecord,
        quota: &UploadQuota,
    ) -> Result<bool, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            if catalog::owner(c, &record.execution_id).await?.as_ref() != Some(scope)
                || &record.scope != scope
            {
                return Err(StoreError::NotFound);
            }
            lock_org(c, scope).await?;
            let existing = c
                .query_opt(
                    "SELECT body FROM runbook.evidence WHERE evidence_id = $1",
                    &[&record.id.as_str()],
                )
                .await
                .map_err(db)?;
            if let Some(row) = existing {
                let existing: EvidenceRecord = decode(row.get::<_, &str>(0))?;
                return if existing == record {
                    Ok(false)
                } else {
                    Err(StoreError::InvalidBatch("upload.duplicate"))
                };
            }
            if record.state != EvidenceState::Pending {
                return Err(StoreError::InvalidBatch("upload.not_pending"));
            }
            if !within_quota(c, &record, quota).await? {
                return Err(StoreError::InvalidBatch("upload.limit_reached"));
            }
            c.execute(
                "INSERT INTO runbook.evidence (evidence_id, org_id, project_id, execution_id, \
                 step_run_id, state, is_image, declared_size, created_at_ms, body) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                &[
                    &record.id.as_str(),
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &record.execution_id.as_str(),
                    &record.step_run_id.as_str(),
                    &label(&record.state)?,
                    &record.declared_type.starts_with("image/"),
                    &int(record.declared_size)?,
                    &record.created_at_ms,
                    &encode(&record)?,
                ],
            )
            .await
            .map_err(db)?;
            Ok(true)
        })
        .await
    }

    async fn get_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> Result<Option<EvidenceRecord>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| read(c, scope, id, false).await)
            .await
    }

    async fn finalize_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
        outcome: UploadOutcome,
        quota: &UploadQuota,
    ) -> Result<EvidenceRecord, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            // The same lock as reserve, so the storage count is stable.
            lock_org(c, scope).await?;
            // ARC2: the run's head before the slot (the order commits and
            // deletions use), so a daily file being recorded for this run
            // waits for the finalize, or the finalize waits for it and then
            // marks it stale: the manifest never silently misses evidence.
            let run = read(c, scope, id, false)
                .await?
                .ok_or(StoreError::NotFound)?
                .execution_id;
            c.query_opt(
                "SELECT 1 FROM runbook.execution WHERE execution_id = $1 FOR UPDATE",
                &[&run.as_str()],
            )
            .await
            .map_err(db)?;
            let mut record = read(c, scope, id, true)
                .await?
                .ok_or(StoreError::NotFound)?;
            if record.state != EvidenceState::Pending {
                return Err(StoreError::InvalidBatch("upload.not_pending"));
            }
            if let (UploadOutcome::Verified(verified), Some(cap)) = (&outcome, quota.storage_bytes)
                && storage_used(c, scope.org_id.as_str(), Some(id.as_str()))
                    .await?
                    .saturating_add(verified.size.saturating_add(verified.thumbnail_bytes))
                    > cap
            {
                return Err(StoreError::InvalidBatch("upload.limit_reached"));
            }
            match outcome {
                UploadOutcome::Verified(verified) => {
                    record.state = EvidenceState::Finalized;
                    record.verified = Some(verified);
                }
                UploadOutcome::Rejected { .. } => record.state = EvidenceState::Rejected,
            }
            record.finalized_at_ms = Some(now_ms(c).await?);
            c.execute(
                "UPDATE runbook.evidence SET state = $2, body = $3 WHERE evidence_id = $1",
                &[&id.as_str(), &label(&record.state)?, &encode(&record)?],
            )
            .await
            .map_err(db)?;
            // ARC2: the manifest lists finalized evidence, so evidence
            // finalized on an archived run makes its file stale.
            if record.state == EvidenceState::Finalized {
                crate::archives::mark_run_stale(c, record.execution_id.as_str()).await?;
            }
            Ok(record)
        })
        .await
    }

    async fn list_evidence(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> Result<Vec<EvidenceRecord>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            c.query(
                "SELECT body FROM runbook.evidence \
                 WHERE org_id = $1 AND project_id = $2 AND execution_id = $3 \
                 ORDER BY created_at_ms, evidence_id COLLATE \"C\"",
                &[
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &execution_id.as_str(),
                ],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|row| decode(row.get::<_, &str>(0)))
            .collect()
        })
        .await
    }

    async fn stale_evidence(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> Result<Vec<EvidenceRecord>, StoreError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.transaction(SNAPSHOT_READ, async |c| {
            c.query(
                "SELECT body FROM runbook.evidence \
                 WHERE org_id = $1 AND project_id = $2 AND state = 'pending' \
                 AND created_at_ms < $3 \
                 ORDER BY created_at_ms, evidence_id COLLATE \"C\" LIMIT $4",
                &[
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &before_ms,
                    &limit,
                ],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|row| decode(row.get::<_, &str>(0)))
            .collect()
        })
        .await
    }

    async fn delete_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> Result<bool, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            Ok(c.execute(
                "DELETE FROM runbook.evidence \
                 WHERE evidence_id = $1 AND org_id = $2 AND project_id = $3",
                &[
                    &id.as_str(),
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                ],
            )
            .await
            .map_err(db)?
                > 0)
        })
        .await
    }
}
