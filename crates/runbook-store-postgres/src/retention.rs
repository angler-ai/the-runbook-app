//! B2a.4 RunStore on PostgreSQL: complete per-run export slices, atomic
//! deletion leaving only a tombstone, and UTC-day finished listing.
use crate::{
    PostgresExecutionStore, catalog,
    execution::{Head, READ_COMMITTED, SNAPSHOT_READ, read_stored},
    rows::{connection, db, decode, encode, uint},
};
use runbook_core::{
    StoreError, WriteStage,
    blob::BlobKey,
    catalog::{Admission, RunCatalogSlice, TenantScope},
    retention::{ExecutionSlice, RunStore, Tombstone, WorkState},
    work_queue::WorkLease,
};
use runbook_model::{ExecutionId, ExecutionStatus, RunbookId};
use tokio_postgres::Client;

fn finished(status: ExecutionStatus) -> bool {
    matches!(
        status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
}

/// Tombstoned executions reject commits and admissions from then on.
pub(crate) async fn tombstoned(c: &Client, id: &ExecutionId) -> Result<bool, StoreError> {
    c.query_one(
        "SELECT EXISTS (SELECT 1 FROM runbook.tombstone WHERE execution_id = $1)",
        &[&id.as_str()],
    )
    .await
    .map(|row| row.get(0))
    .map_err(db)
}

/// Head row for this tenant inside the caller's transaction, as in load().
async fn scoped_head(
    c: &Client,
    scope: &TenantScope,
    id: &ExecutionId,
) -> Result<Head, StoreError> {
    let row = c
        .query_opt(
            "SELECT e.revision, e.projection FROM runbook.execution e \
             JOIN runbook.execution_owner o USING (execution_id) \
             WHERE e.execution_id = $1 AND o.org_id = $2 AND o.project_id = $3",
            &[
                &id.as_str(),
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
            ],
        )
        .await
        .map_err(db)?
        .ok_or(StoreError::NotFound)?;
    Ok(Head {
        revision: uint(row.get(0))?,
        projection: decode(row.get(1))?,
    })
}

/// One run's complete slice inside the caller's snapshot, open or finished
/// (the D3 project archive exports open runs; B2.4 retention refuses them).
pub(crate) async fn read_slice(
    c: &Client,
    scope: &TenantScope,
    id: &ExecutionId,
) -> Result<ExecutionSlice, StoreError> {
    if catalog::owner(c, id).await?.as_ref() != Some(scope) {
        return Err(StoreError::NotFound);
    }
    let run = read_stored(c, scoped_head(c, scope, id).await?).await?;
    let summary = c
        .query_opt(
            "SELECT body FROM runbook.run_summary WHERE execution_id = $1",
            &[&id.as_str()],
        )
        .await
        .map_err(db)?
        .map(|row| decode(row.get(0)))
        .transpose()?;
    let mut assignments = Vec::new();
    for row in c
        .query(
            "SELECT body FROM runbook.assignment \
             WHERE execution_id = $1 ORDER BY ordinal",
            &[&id.as_str()],
        )
        .await
        .map_err(db)?
    {
        assignments.push(decode(row.get(0))?);
    }
    let admission = c
        .query_opt(
            "SELECT request, state, admitted_at_ms FROM runbook.admission WHERE execution_id = $1",
            &[&id.as_str()],
        )
        .await
        .map_err(db)?
        .map(|row| {
            Ok((
                decode(row.get(0))?,
                Admission {
                    execution_id: id.clone(),
                    state: catalog::state_of(row.get(1))?,
                    admitted_at_ms: row.get(2),
                },
            ))
        })
        .transpose()?;
    let mut work = Vec::new();
    for row in c
        .query(
            "SELECT body, acknowledged FROM runbook.work_lease \
             WHERE org_id = $1 AND project_id = $2 AND execution_id = $3",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &id.as_str(),
            ],
        )
        .await
        .map_err(db)?
    {
        let lease: WorkLease = decode(row.get(0))?;
        work.push(WorkState {
            lease,
            acknowledged: row.get(1),
        });
    }
    work.sort_by(|a, b| a.lease.id.cmp(&b.lease.id));
    let mut blob_keys = Vec::new();
    for row in c
        .query(
            "SELECT name FROM runbook.blob \
             WHERE org_id = $1 AND project_id = $2 AND execution_id = $3",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &id.as_str(),
            ],
        )
        .await
        .map_err(db)?
    {
        blob_keys.push(BlobKey::Execution {
            execution_id: id.clone(),
            name: row.get(0),
        });
    }
    blob_keys.sort();
    Ok(ExecutionSlice {
        scope: scope.clone(),
        actor_replays: crate::actor_replay::export(c, id).await?,
        run,
        catalog: RunCatalogSlice {
            summary,
            assignments,
            admission,
        },
        work,
        blob_keys,
    })
}

/// A run's tombstone, if any: the tombstone itself when it is in `scope`,
/// NotFound when another tenant's.
async fn existing_tombstone(
    c: &Client,
    scope: &TenantScope,
    id: &ExecutionId,
) -> Result<Option<Result<Tombstone, StoreError>>, StoreError> {
    let Some(row) = c
        .query_opt(
            "SELECT body FROM runbook.tombstone WHERE execution_id = $1",
            &[&id.as_str()],
        )
        .await
        .map_err(db)?
    else {
        return Ok(None);
    };
    let existing: Tombstone = decode(row.get(0))?;
    Ok(Some(if &existing.scope == scope {
        Ok(existing)
    } else {
        Err(StoreError::NotFound)
    }))
}

impl RunStore for PostgresExecutionStore {
    async fn export_execution(&self, id: &ExecutionId) -> Result<ExecutionSlice, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            let slice = read_slice(c, &self.scope, id).await?;
            if !finished(slice.run.projection.status) {
                return Err(StoreError::InvalidBatch("store.execution_open"));
            }
            Ok(slice)
        })
        .await
    }

    async fn delete_execution(
        &self,
        id: &ExecutionId,
        reason: &str,
    ) -> Result<Tombstone, StoreError> {
        if reason.trim().is_empty() {
            return Err(StoreError::InvalidBatch("store.deletion_reason_required"));
        }
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(READ_COMMITTED).await.map_err(db)?;
        let result = async {
            let c = conn.client();
            if let Some(existing) = existing_tombstone(c, &self.scope, id).await? {
                // A single-run deletion of a run retention already deleted
                // takes it out of its file too (never a silent no-op).
                if let Ok(tombstone) = &existing
                    && tombstone.reason == runbook_core::archives::RETENTION_REASON
                    && reason != runbook_core::archives::RETENTION_REASON
                {
                    // A concurrent single-run deletion that took the
                    // tombstone first: return its tombstone, as a repeat
                    // deletion does.
                    return match crate::archives::purge_retained(c, &self.scope, id, reason).await?
                    {
                        Some(tombstone) => Ok(tombstone),
                        None => existing_tombstone(c, &self.scope, id)
                            .await?
                            .unwrap_or(Err(StoreError::NotFound)),
                    };
                }
                return existing;
            }
            if catalog::owner(c, id).await?.as_ref() != Some(&self.scope) {
                return Err(StoreError::NotFound);
            }
            // Lock the head so a concurrent commit cannot interleave.
            let Some(head) = self.lock_head(c, id).await? else {
                // F38: a concurrent deletion won while this one waited; its
                // tombstone is committed now, so return it as memory does.
                return match existing_tombstone(c, &self.scope, id).await? {
                    Some(existing) => existing,
                    None => Err(StoreError::NotFound),
                };
            };
            if !finished(head.projection.status) {
                return Err(StoreError::InvalidBatch("store.execution_open"));
            }
            // ARC2: retention deletes live detail only while the run's file
            // is current and holds this revision (rechecked here, under the
            // head lock, not from the job's earlier snapshot).
            let retention = reason == runbook_core::archives::RETENTION_REASON;
            if retention
                && !crate::archives::archived_current(c, &self.scope, id, head.revision).await?
            {
                return Err(StoreError::InvalidBatch("archive.not_current"));
            }
            if let Some(row) = c
                .query_opt(
                    "SELECT state, queue_order FROM runbook.admission \
                     WHERE execution_id = $1 FOR UPDATE",
                    &[&id.as_str()],
                )
                .await
                .map_err(db)?
            {
                if catalog::state_of(row.get(0))? != runbook_core::catalog::AdmissionState::Released
                {
                    return Err(StoreError::InvalidBatch("catalog.admission_not_released"));
                }
                if row.get::<_, Option<i64>>(1).is_some() {
                    return Err(StoreError::InvalidBatch(
                        "catalog.released_admission_queued",
                    ));
                }
            }
            let deleted_at_ms: i64 = c
                .query_one(
                    "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                    &[],
                )
                .await
                .map_err(db)?
                .get(0);
            let tombstone = Tombstone {
                scope: self.scope.clone(),
                execution_id: id.clone(),
                runbook_id: head.projection.runbook_id.clone(),
                deleted_at_ms,
                reason: reason.into(),
            };
            // F46: step-run IDs outlive the run, like its tombstone.
            c.execute(
                "INSERT INTO runbook.step_run_tombstone \
                 (step_run_id, execution_id, org_id, project_id) \
                 SELECT step_run_id, execution_id, org_id, project_id \
                 FROM runbook.step_run_index WHERE execution_id = $1 \
                 ON CONFLICT DO NOTHING",
                &[&id.as_str()],
            )
            .await
            .map_err(db)?;
            // PRD §14: retention deletes only the live copy; the run's
            // uploads (blob rows without a name) follow its archive file, so
            // they move to the runbook and outlive the run's owner row.
            if retention {
                // Their bytes keep counting toward storage until the file
                // expires (the evidence rows go with the run below).
                c.execute(
                    &format!(
                        "INSERT INTO runbook.retained_evidence \
                         (evidence_id, org_id, project_id, execution_id, counted_bytes) \
                         SELECT evidence_id, org_id, project_id, execution_id, {} \
                         FROM runbook.evidence WHERE execution_id = $1 AND state <> 'rejected' \
                         ON CONFLICT DO NOTHING",
                        crate::uploads::COUNTED_BYTES
                    ),
                    &[&id.as_str()],
                )
                .await
                .map_err(db)?;
                c.execute(
                    "UPDATE runbook.blob SET execution_id = NULL, runbook_id = $4 \
                     WHERE org_id = $1 AND project_id = $2 AND execution_id = $3 \
                     AND name IS NULL AND hash IS NULL",
                    &[
                        &self.scope.org_id.as_str(),
                        &self.scope.project_id.as_str(),
                        &id.as_str(),
                        &head.projection.runbook_id.as_str(),
                    ],
                )
                .await
                .map_err(db)?;
            }
            // Every run-owned family, by run-owned keys; the shared blob
            // namespace and sibling runs are untouched.
            for sql in [
                "DELETE FROM runbook.work_lease \
                 WHERE org_id = $1 AND project_id = $2 AND execution_id = $3",
                "DELETE FROM runbook.blob \
                 WHERE org_id = $1 AND project_id = $2 AND execution_id = $3",
            ] {
                c.execute(
                    sql,
                    &[
                        &self.scope.org_id.as_str(),
                        &self.scope.project_id.as_str(),
                        &id.as_str(),
                    ],
                )
                .await
                .map_err(db)?;
            }
            for table in [
                "claim_log",
                "start_replay",
                "execution_start",
                "start_idempotency",
                "admission",
                "assignment",
                "run_summary",
                "execution_derived_outbox",
                "execution_outbox",
                "execution_idempotency",
                "actor_replay",
                "step_run_index",
                "execution_claim",
                "execution_timer",
                "execution_timer_history",
                "execution_event",
                "execution",
                "evidence",
                "execution_owner",
                "email_delivery",
            ] {
                c.execute(
                    &format!("DELETE FROM runbook.{table} WHERE execution_id = $1"),
                    &[&id.as_str()],
                )
                .await
                .map_err(db)?;
            }
            crate::webhooks::delete_run_rows(c, &self.scope, id).await?;
            // ARC2: retention keeps the summary row (the archive is the
            // copy); any other deletion takes the run out of its file.
            crate::archives::on_deletion(c, id.as_str(), reason).await?;
            self.checkpoint(WriteStage::RunDeletion)?;
            c.execute(
                "INSERT INTO runbook.tombstone (execution_id, org_id, project_id, body) \
                 VALUES ($1, $2, $3, $4)",
                &[
                    &id.as_str(),
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &encode(&tombstone)?,
                ],
            )
            .await
            .map_err(db)?;
            Ok(tombstone)
        }
        .await;
        crate::execution::finish(&mut conn, result).await
    }

    async fn tombstone(&self, id: &ExecutionId) -> Result<Option<Tombstone>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT body FROM runbook.tombstone \
                 WHERE execution_id = $1 AND org_id = $2 AND project_id = $3",
                &[
                    &id.as_str(),
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                ],
            )
            .await
            .map_err(db)?
            .map(|row| decode(row.get(0)))
            .transpose()
    }

    async fn finished_on(
        &self,
        runbook: &RunbookId,
        utc_day: i64,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        let start = utc_day
            .checked_mul(86_400_000)
            .ok_or(StoreError::InvalidBatch("store.day_overflow"))?;
        let end = start
            .checked_add(86_400_000)
            .ok_or(StoreError::InvalidBatch("store.day_overflow"))?;
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query(
                "SELECT e.execution_id FROM runbook.execution e \
                 JOIN runbook.execution_owner o USING (execution_id) \
                 WHERE o.org_id = $1 AND o.project_id = $2 AND e.runbook_id = $3 \
                 AND e.status IN ('succeeded', 'failed', 'cancelled') \
                 AND e.completed_at_ms >= $4 AND e.completed_at_ms < $5 \
                 ORDER BY e.completed_at_ms, e.execution_id",
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &runbook.as_str(),
                    &start,
                    &end,
                ],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|row| {
                row.get::<_, &str>(0)
                    .parse()
                    .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))
            })
            .collect()
    }
}
