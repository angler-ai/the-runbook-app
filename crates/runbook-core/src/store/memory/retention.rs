use super::*;
use crate::retention::{ExecutionSlice, RunStore, Tombstone, WorkState};
use runbook_model::RunbookId;
fn finished(run: &StoredExecution) -> bool {
    matches!(
        run.projection.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
}
/// One run's complete slice, open or finished (the D3 project archive
/// exports open runs too; the B2.4 retention export refuses them).
pub(super) fn slice(
    db: &Database,
    scope: &TenantScope,
    id: &ExecutionId,
) -> Result<ExecutionSlice, StoreError> {
    if db.owners.get(id) != Some(scope) {
        return Err(StoreError::NotFound);
    }
    let run = db.runs.get(id).ok_or(StoreError::NotFound)?;
    Ok(ExecutionSlice {
        scope: scope.clone(),
        run: run.clone(),
        actor_replays: db
            .actor_replays
            .iter()
            .filter(|((execution, _, _), _)| execution == id)
            .map(|(_, row)| row.clone())
            .collect(),
        catalog: db.catalog.export_run(scope, id)?,
        work: db
            .work_leases
            .iter()
            .filter(|((owner, key), _)| owner == scope && &key.execution_id == id)
            .map(|(_, row)| WorkState {
                lease: row.lease.clone(),
                acknowledged: row.acknowledged,
            })
            .collect(),
        blob_keys: db
            .blobs
            .keys()
            .filter(|(owner, key)| {
                owner == scope
                    && matches!(key,BlobKey::Execution{execution_id,..}if execution_id==id)
            })
            .map(|(_, key)| key.clone())
            .collect(),
    })
}
impl RunStore for MemoryExecutionStore {
    async fn export_execution(&self, id: &ExecutionId) -> Result<ExecutionSlice, StoreError> {
        let db = self.database.borrow();
        let slice = slice(&db, &self.scope, id)?;
        if !finished(&slice.run) {
            return Err(StoreError::InvalidBatch("store.execution_open"));
        }
        Ok(slice)
    }
    async fn delete_execution(
        &self,
        id: &ExecutionId,
        reason: &str,
    ) -> Result<Tombstone, StoreError> {
        if reason.trim().is_empty() {
            return Err(StoreError::InvalidBatch("store.deletion_reason_required"));
        }
        let now = self.clock.get();
        self.transaction(|db| {
            if let Some(t) = db.tombstones.get(id) {
                if t.scope != self.scope {
                    return Err(StoreError::NotFound);
                }
                // A single-run deletion of a run retention already deleted
                // takes it out of its file too (never a silent no-op).
                if t.reason == crate::archives::RETENTION_REASON
                    && reason != crate::archives::RETENTION_REASON
                {
                    return super::archives::purge_retained(db, &self.scope, id, reason)
                        .ok_or(StoreError::NotFound);
                }
                return Ok(t.clone());
            }
            if db.owners.get(id) != Some(&self.scope) {
                return Err(StoreError::NotFound);
            }
            let run = db.runs.get(id).ok_or(StoreError::NotFound)?;
            if !finished(run) {
                return Err(StoreError::InvalidBatch("store.execution_open"));
            }
            let retention = reason == crate::archives::RETENTION_REASON;
            // ARC2: retention deletes live detail only while the run's file
            // is current and holds this revision (rechecked here, under the
            // run's transaction, not from the job's earlier snapshot).
            if retention && !super::archives::archived_current(db, &self.scope, id, run.revision) {
                return Err(StoreError::InvalidBatch("archive.not_current"));
            }
            let tombstone = Tombstone {
                scope: self.scope.clone(),
                execution_id: id.clone(),
                runbook_id: run.projection.runbook_id.clone(),
                deleted_at_ms: now,
                reason: reason.into(),
            };
            db.catalog.delete_run(&self.scope, id)?;
            db.runs.remove(id);
            db.actor_replays
                .retain(|(execution, _, _), _| execution != id);
            db.owners.remove(id);
            db.work_leases
                .retain(|(scope, key), _| scope != &self.scope || &key.execution_id != id);
            db.starts.remove(id);
            db.credential_families
                .retain(|_, (_, family)| &family.execution_id != id);
            db.exec_credentials
                .retain(|_, record| &record.execution_id != id);
            db.claim_logs
                .retain(|(scope, execution_id, _), _| scope != &self.scope || execution_id != id);
            db.start_replays.retain(|(scope, _), (replay, _)| {
                scope != &self.scope || &replay.outcome.execution_id != id
            });
            // PRD §14: retention deletes only the live copy; the run's
            // uploads follow its archive file (deleted when the file
            // expires or on a single-run deletion).
            db.blobs.retain(|(scope, key), _| {
                scope != &self.scope
                    || !super::io::in_run(key, id)
                    || (retention && matches!(key, BlobKey::Upload { .. }))
            });
            // UP1: the run's evidence slots go with it (their objects above).
            // ARC2: retention keeps the objects, so their bytes keep counting
            // toward storage until the file expires (PRD §14).
            let slots: Vec<_> = db
                .evidence
                .iter()
                .filter(|(_, r)| r.scope == self.scope && &r.execution_id == id)
                .map(|(k, _)| k.clone())
                .collect();
            for key in slots {
                if let Some(record) = db.evidence.remove(&key)
                    && retention
                    && record.state != crate::uploads::EvidenceState::Rejected
                {
                    db.retained_evidence.insert(key, record);
                }
            }
            // ARC2: retention keeps the run's summary row (its archive is
            // the copy); any other deletion takes the run out of its file:
            // the row goes and the file is marked stale for a rewrite.
            if !retention {
                super::archives::mark_run_stale(db, id);
                db.archived_runs.remove(id);
            }
            self.checkpoint(WriteStage::RunDeletion)?;
            db.tombstones.insert(id.clone(), tombstone.clone());
            Ok(tombstone)
        })
    }
    async fn tombstone(&self, id: &ExecutionId) -> Result<Option<Tombstone>, StoreError> {
        Ok(self
            .database
            .borrow()
            .tombstones
            .get(id)
            .filter(|t| t.scope == self.scope)
            .cloned())
    }
    async fn finished_on(
        &self,
        runbook: &RunbookId,
        day: i64,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        let start = day
            .checked_mul(86_400_000)
            .ok_or(StoreError::InvalidBatch("store.day_overflow"))?;
        let end = start
            .checked_add(86_400_000)
            .ok_or(StoreError::InvalidBatch("store.day_overflow"))?;
        let db = self.database.borrow();
        let mut rows: Vec<_> = db
            .runs
            .iter()
            .filter(|(id, run)| {
                db.owners.get(*id) == Some(&self.scope)
                    && &run.projection.runbook_id == runbook
                    && finished(run)
            })
            .filter_map(|(id, run)| {
                run.projection
                    .completed_at_ms
                    .filter(|time| *time >= start && *time < end)
                    .map(|time| (time, id.clone()))
            })
            .collect();
        rows.sort();
        Ok(rows.into_iter().map(|(_, id)| id).collect())
    }
}

impl MemoryExecutionStore {
    /// Conformance-only raw inventory, bypassing read masks to expose orphan rows.
    /// Adapters must enumerate every run-owned table; this is not a public API.
    pub fn owned_record_counts(&self, id: &ExecutionId) -> BTreeMap<String, usize> {
        let db = self.database.borrow();
        let run = db.runs.get(id);
        let mut counts = db.catalog.owned_record_counts(id);
        for (table, count) in [
            ("execution_head", usize::from(run.is_some())),
            ("execution_owner", usize::from(db.owners.contains_key(id))),
            ("event", run.map_or(0, |r| r.events.len())),
            (
                "step_projection",
                run.map_or(0, |r| r.projection.steps.len()),
            ),
            ("timer", run.map_or(0, |r| r.timers.len())),
            ("timer_history", run.map_or(0, |r| r.timer_history.len())),
            ("outbox", run.map_or(0, |r| r.outbox.len())),
            ("derived_outbox", run.map_or(0, |r| r.derived_outbox.len())),
            ("idempotency", run.map_or(0, |r| r.idempotency.len())),
            ("claim", run.map_or(0, |r| r.claims.len())),
            (
                "work_lease",
                db.work_leases
                    .keys()
                    .filter(|(_, key)| &key.execution_id == id)
                    .count(),
            ),
            ("start_record", usize::from(db.starts.contains_key(id))),
            (
                "claim_log",
                db.claim_logs
                    .iter()
                    .filter(|((_, execution_id, _), _)| execution_id == id)
                    .map(|(_, lines)| lines.len())
                    .sum(),
            ),
            (
                "credential_family",
                db.credential_families
                    .values()
                    .filter(|(_, f)| &f.execution_id == id)
                    .count(),
            ),
            (
                "execution_credential",
                db.exec_credentials
                    .values()
                    .filter(|r| &r.execution_id == id)
                    .count(),
            ),
            (
                "start_replay",
                db.start_replays
                    .values()
                    .filter(|(r, _)| &r.outcome.execution_id == id)
                    .count(),
            ),
            (
                "blob",
                db.blobs
                    .keys()
                    .filter(|(_, key)| super::io::in_run(key, id))
                    .count(),
            ),
            (
                "blob_chunk",
                db.blobs
                    .iter()
                    .filter(|((_, key), _)| super::io::in_run(key, id))
                    .map(|(_, chunks)| chunks.len())
                    .sum(),
            ),
            (
                "evidence",
                db.evidence
                    .values()
                    .filter(|r| &r.execution_id == id)
                    .count(),
            ),
            ("tombstone", usize::from(db.tombstones.contains_key(id))),
            (
                "archive_run",
                usize::from(db.archived_runs.contains_key(id)),
            ),
        ] {
            counts.insert(table.into(), count);
        }
        counts
    }
}
