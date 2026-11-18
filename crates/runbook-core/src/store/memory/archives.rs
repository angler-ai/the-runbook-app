//! ARC1/ARC2 `ArchiveIndex` for the reference memory store. Every call is
//! one synchronous transaction filtered by tenant, so the races the
//! Postgres adapter serializes with locks cannot interleave here; the
//! checks are the same.
use super::{Database, MemoryExecutionStore};
use crate::{
    StoreError,
    archives::{
        ArchiveIndex, ArchiveRecord, ArchivedRun, RETENTION_REASON, VERSION_GRACE_MS, backoff_ms,
        day_of,
    },
    blob::BlobKey,
    catalog::TenantScope,
    retention::Tombstone,
    uploads::EvidenceState,
};
use runbook_model::{EvidenceId, ExecutionId, ExecutionStatus, RunbookId};

type File = (TenantScope, RunbookId, i64);

fn finished(db: &Database, scope: &TenantScope, id: &ExecutionId) -> Option<i64> {
    if db.owners.get(id) != Some(scope) {
        return None;
    }
    let run = db.runs.get(id)?;
    matches!(
        run.projection.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
    .then_some(run.projection.completed_at_ms)
    .flatten()
}

fn file_of(scope: &TenantScope, run: &ArchivedRun) -> File {
    (scope.clone(), run.runbook_id.clone(), run.day)
}

fn key_of(record: &ArchiveRecord) -> BlobKey {
    BlobKey::Archive {
        runbook_id: record.runbook_id.clone(),
        day: record.day,
        digest: record.digest,
    }
}

/// Mark the file that lists `id` stale, if any (inside a caller's
/// transaction: a commit to the run, finalized evidence, its deletion).
/// Found through membership, which outlives the summary row.
pub(super) fn mark_run_stale(db: &mut Database, id: &ExecutionId) {
    if let Some(file) = db.archive_members.get(id).cloned()
        && let Some(record) = db.archives.get_mut(&file)
    {
        record.stale = true;
        record.stale_marks += 1;
    }
}

/// Retention may delete `id` only while a stored, not stale file lists it
/// at `revision` (inside the deletion's transaction).
pub(super) fn archived_current(
    db: &Database,
    scope: &TenantScope,
    id: &ExecutionId,
    revision: u64,
) -> bool {
    db.archived_runs
        .get(id)
        .filter(|(owner, run)| owner == scope && run.revision == revision)
        .and_then(|(owner, run)| db.archives.get(&file_of(owner, run)))
        .is_some_and(|record| {
            !record.stale && db.blobs.contains_key(&(scope.clone(), key_of(record)))
        })
}

/// The finalized evidence of a live run, sorted (what its file must list).
fn finalized(db: &Database, scope: &TenantScope, id: &ExecutionId) -> Vec<EvidenceId> {
    let mut out: Vec<EvidenceId> = db
        .evidence
        .values()
        .filter(|r| {
            &r.scope == scope && &r.execution_id == id && r.state == EvidenceState::Finalized
        })
        .map(|r| r.id.clone())
        .collect();
    out.sort();
    out
}

/// Delete a run's retained uploads (every `Upload` key under the run) and
/// their counted bytes.
fn drop_retained_uploads(db: &mut Database, scope: &TenantScope, id: &ExecutionId) {
    db.blobs.retain(|(owner, key), _| {
        owner != scope || !matches!(key, BlobKey::Upload { execution_id, .. } if execution_id == id)
    });
    db.retained_evidence
        .retain(|_, r| &r.scope != scope || &r.execution_id != id);
}

/// A single-run deletion of a run retention already deleted (inside a
/// transaction). `None` when `id` has no retention tombstone in `scope`.
pub(super) fn purge_retained(
    db: &mut Database,
    scope: &TenantScope,
    id: &ExecutionId,
    reason: &str,
) -> Option<Tombstone> {
    let tombstone = db
        .tombstones
        .get_mut(id)
        .filter(|t| &t.scope == scope && t.reason == RETENTION_REASON)?;
    tombstone.reason = reason.into();
    let tombstone = tombstone.clone();
    mark_run_stale(db, id);
    db.archived_runs.remove(id);
    drop_retained_uploads(db, scope, id);
    Some(tombstone)
}

/// Delete stored versions of `file` the index does not name that are older
/// than the named one, or than the grace period: never a younger in-flight
/// write.
fn sweep(db: &mut Database, file: &File, now_ms: i64) -> usize {
    let named = db.archives.get(file).map(key_of);
    let named_at = named
        .as_ref()
        .and_then(|k| db.archive_versions.get(&(file.0.clone(), k.clone())))
        .copied();
    let doomed: Vec<(TenantScope, BlobKey)> = db
        .archive_versions
        .iter()
        .filter(|((owner, key), (seq, at))| {
            owner == &file.0
                && matches!(key, BlobKey::Archive { runbook_id, day, .. }
                    if runbook_id == &file.1 && *day == file.2)
                && Some(key) != named.as_ref()
                && (named_at.is_some_and(|(n, _)| *seq < n) || *at < now_ms - VERSION_GRACE_MS)
        })
        .map(|(k, _)| k.clone())
        .collect();
    let swept = doomed.len();
    for key in doomed {
        db.blobs.remove(&key);
        db.archive_versions.remove(&key);
    }
    swept
}

fn backing_off(db: &Database, file: &File, now_ms: i64) -> bool {
    db.archive_backoff
        .get(file)
        .is_some_and(|(_, next)| *next > now_ms)
}

impl ArchiveIndex for MemoryExecutionStore {
    async fn record_archive(
        &self,
        scope: &TenantScope,
        record: ArchiveRecord,
        runs: Vec<ArchivedRun>,
        expected: Option<[u8; 32]>,
    ) -> Result<(), StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            if &record.scope != scope
                || runs
                    .iter()
                    .any(|r| r.runbook_id != record.runbook_id || r.day != record.day)
            {
                return Err(StoreError::InvalidBatch("archive.foreign_run"));
            }
            let mut stale = false;
            let mut keep = Vec::new();
            let mut members = Vec::new();
            for run in runs {
                members.push(run.execution_id.clone());
                let head = db
                    .runs
                    .get(&run.execution_id)
                    .filter(|_| db.owners.get(&run.execution_id) == Some(scope));
                if let Some(head) = head {
                    let mut listed = run.evidence.clone();
                    listed.sort();
                    stale |= head.revision != run.revision
                        || listed != finalized(db, scope, &run.execution_id);
                    keep.push(run);
                    continue;
                }
                let retained = db
                    .tombstones
                    .get(&run.execution_id)
                    .is_some_and(|t| &t.scope == scope && t.reason == RETENTION_REASON);
                if !retained {
                    return Err(StoreError::InvalidBatch("archive.run_gone"));
                }
                // Built from the live run, which retention has deleted since:
                // the writer starts over and copies the stored folder.
                if run.gathered_live {
                    return Err(StoreError::InvalidBatch("archive.conflict"));
                }
                // A purged summary row is not restored by a rewrite.
                if db
                    .archived_runs
                    .get(&run.execution_id)
                    .is_some_and(|(owner, _)| owner == scope)
                {
                    keep.push(run);
                }
            }
            let file = (scope.clone(), record.runbook_id.clone(), record.day);
            let current = db.archives.get(&file);
            if current.map(|r| r.digest) != expected {
                return Err(StoreError::InvalidBatch("archive.conflict"));
            }
            if !db.blobs.contains_key(&(scope.clone(), key_of(&record))) {
                return Err(StoreError::InvalidBatch("archive.blob_missing"));
            }
            // A mark since the writer read the row keeps the file stale.
            let marks = current.map_or(0, |r| r.stale_marks);
            stale |= marks != record.stale_marks;
            let listed: Vec<&ExecutionId> = keep.iter().map(|r| &r.execution_id).collect();
            db.archived_runs.retain(|id, (owner, run)| {
                !(owner == scope
                    && run.runbook_id == record.runbook_id
                    && run.day == record.day
                    && !listed.contains(&id))
            });
            for run in keep {
                db.archived_runs
                    .insert(run.execution_id.clone(), (scope.clone(), run));
            }
            db.archive_members.retain(|_, f| f != &file);
            for id in members {
                db.archive_members.insert(id, file.clone());
            }
            db.archive_backoff.remove(&file);
            db.archives.insert(
                file.clone(),
                ArchiveRecord {
                    stale,
                    stale_marks: marks,
                    ..record
                },
            );
            sweep(db, &file, now);
            Ok(())
        })
    }
    async fn mark_archive_stale(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<bool, StoreError> {
        self.transaction(|db| {
            Ok(
                match db
                    .archives
                    .get_mut(&(scope.clone(), runbook_id.clone(), day))
                {
                    Some(record) => {
                        record.stale_marks += 1;
                        !std::mem::replace(&mut record.stale, true)
                    }
                    None => false,
                },
            )
        })
    }
    async fn get_archive(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<Option<ArchiveRecord>, StoreError> {
        Ok(self
            .database
            .borrow()
            .archives
            .get(&(scope.clone(), runbook_id.clone(), day))
            .cloned())
    }
    async fn list_archives(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        from_day: Option<i64>,
        to_day: Option<i64>,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        let db = self.database.borrow();
        let mut out: Vec<ArchiveRecord> = db
            .archives
            .values()
            .filter(|r| {
                &r.scope == scope
                    && &r.runbook_id == runbook_id
                    && from_day.is_none_or(|from| r.day >= from)
                    && to_day.is_none_or(|to| r.day <= to)
            })
            .cloned()
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.day));
        Ok(out)
    }
    async fn archived_run(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> Result<Option<ArchivedRun>, StoreError> {
        Ok(self
            .database
            .borrow()
            .archived_runs
            .get(execution_id)
            .filter(|(owner, _)| owner == scope)
            .map(|(_, run)| run.clone()))
    }
    async fn stale_archives(
        &self,
        scope: &TenantScope,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        let db = self.database.borrow();
        let mut out: Vec<ArchiveRecord> = db
            .archives
            .iter()
            .filter(|(file, r)| &r.scope == scope && r.stale && !backing_off(&db, file, now_ms))
            .map(|(_, r)| r.clone())
            .collect();
        out.sort_by(|a, b| (a.day, &a.runbook_id).cmp(&(b.day, &b.runbook_id)));
        out.truncate(limit);
        Ok(out)
    }
    async fn delete_archive(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
        expected: [u8; 32],
    ) -> Result<bool, StoreError> {
        self.transaction(|db| {
            let file = (scope.clone(), runbook_id.clone(), day);
            match db.archives.get(&file) {
                None => return Ok(false),
                Some(r) if r.digest != expected => {
                    return Err(StoreError::InvalidBatch("archive.conflict"));
                }
                Some(_) => {}
            }
            let live = db.archive_members.iter().any(|(id, f)| {
                f == &file && db.owners.get(id) == Some(scope) && db.runs.contains_key(id)
            });
            if live {
                return Err(StoreError::InvalidBatch("archive.member_live"));
            }
            db.archive_backoff.remove(&file);
            let found = db.archives.remove(&file).is_some();
            // Every stored version of the file.
            let versions: Vec<(TenantScope, BlobKey)> = db
                .archive_versions
                .keys()
                .filter(|(owner, key)| {
                    owner == scope
                        && matches!(key, BlobKey::Archive { runbook_id: r, day: d, .. }
                            if r == runbook_id && *d == day)
                })
                .cloned()
                .collect();
            for key in versions {
                db.blobs.remove(&key);
                db.archive_versions.remove(&key);
            }
            // The member runs' retained uploads follow the file (PRD §14).
            let members: Vec<ExecutionId> = db
                .archive_members
                .iter()
                .filter(|(_, f)| *f == &file)
                .map(|(id, _)| id.clone())
                .collect();
            for id in members {
                db.archive_members.remove(&id);
                let retained = db
                    .tombstones
                    .get(&id)
                    .is_some_and(|t| &t.scope == scope && t.reason == RETENTION_REASON);
                if retained {
                    drop_retained_uploads(db, scope, &id);
                }
            }
            Ok(found)
        })
    }
    async fn unarchived_days(
        &self,
        scope: &TenantScope,
        before_day: i64,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<(RunbookId, i64)>, StoreError> {
        let db = self.database.borrow();
        let mut days: Vec<(i64, RunbookId)> = db
            .runs
            .iter()
            .filter(|(id, _)| !db.archived_runs.contains_key(*id))
            .filter_map(|(id, run)| {
                let day = day_of(finished(&db, scope, id)?);
                (day < before_day).then(|| (day, run.projection.runbook_id.clone()))
            })
            .filter(|(day, rb)| !backing_off(&db, &(scope.clone(), rb.clone(), *day), now_ms))
            .collect();
        days.sort();
        days.dedup();
        days.truncate(limit);
        Ok(days.into_iter().map(|(day, rb)| (rb, day)).collect())
    }
    async fn archived_live_runs(
        &self,
        scope: &TenantScope,
        completed_before_ms: i64,
        limit: usize,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        let db = self.database.borrow();
        let mut out: Vec<(i64, ExecutionId)> = db
            .archived_runs
            .iter()
            .filter(|(_, (owner, run))| {
                owner == scope
                    && db
                        .archives
                        .get(&file_of(owner, run))
                        .is_some_and(|r| !r.stale)
            })
            .filter_map(|(id, _)| {
                let at = finished(&db, scope, id)?;
                (at < completed_before_ms).then(|| (at, id.clone()))
            })
            .collect();
        out.sort();
        out.truncate(limit);
        Ok(out.into_iter().map(|(_, id)| id).collect())
    }
    async fn expired_archives(
        &self,
        scope: &TenantScope,
        before_day: i64,
        limit: usize,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        let db = self.database.borrow();
        let mut out: Vec<ArchiveRecord> = db
            .archives
            .values()
            .filter(|r| &r.scope == scope && r.day < before_day)
            .cloned()
            .collect();
        out.sort_by(|a, b| (a.day, &a.runbook_id).cmp(&(b.day, &b.runbook_id)));
        out.truncate(limit);
        Ok(out)
    }
    async fn purge_summaries(
        &self,
        scope: &TenantScope,
        completed_before_ms: i64,
        limit: usize,
    ) -> Result<usize, StoreError> {
        self.transaction(|db| {
            let mut doomed: Vec<(i64, ExecutionId)> = db
                .archived_runs
                .iter()
                .filter(|(id, (owner, run))| {
                    owner == scope
                        && run.summary.completed_at_ms < completed_before_ms
                        && !db.runs.contains_key(*id)
                })
                .map(|(id, (_, run))| (run.summary.completed_at_ms, id.clone()))
                .collect();
            doomed.sort();
            doomed.truncate(limit);
            for (_, id) in &doomed {
                db.archived_runs.remove(id);
            }
            Ok(doomed.len())
        })
    }
    async fn archive_failed(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
        now_ms: i64,
    ) -> Result<u32, StoreError> {
        self.transaction(|db| {
            let entry = db
                .archive_backoff
                .entry((scope.clone(), runbook_id.clone(), day))
                .or_insert((0, now_ms));
            entry.0 = entry.0.saturating_add(1);
            entry.1 = now_ms.saturating_add(backoff_ms(entry.0));
            Ok(entry.0)
        })
    }
    async fn file_members(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        let file = (scope.clone(), runbook_id.clone(), day);
        Ok(self
            .database
            .borrow()
            .archive_members
            .iter()
            .filter(|(_, f)| *f == &file)
            .map(|(id, _)| id.clone())
            .collect())
    }
    async fn sweep_leftover_versions(
        &self,
        scope: &TenantScope,
        limit: usize,
    ) -> Result<usize, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            let mut days: Vec<File> = db
                .archive_versions
                .iter()
                .filter(|((owner, _), (_, at))| owner == scope && *at < now - VERSION_GRACE_MS)
                .filter_map(|((_, key), _)| match key {
                    BlobKey::Archive {
                        runbook_id, day, ..
                    } => {
                        let file = (scope.clone(), runbook_id.clone(), *day);
                        let named = db.archives.get(&file).map(key_of);
                        (named.as_ref() != Some(key)).then_some(file)
                    }
                    _ => None,
                })
                .collect();
            days.sort();
            days.dedup();
            days.truncate(limit);
            Ok(days.iter().map(|file| sweep(db, file, now)).sum())
        })
    }
    async fn sweep_archive_versions(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<(), StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            sweep(db, &(scope.clone(), runbook_id.clone(), day), now);
            Ok(())
        })
    }
    async fn purge_retained_run(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
        reason: &str,
    ) -> Result<Option<Tombstone>, StoreError> {
        if reason.trim().is_empty() || reason == RETENTION_REASON {
            return Err(StoreError::InvalidBatch("archive.reserved_reason"));
        }
        self.transaction(|db| Ok(purge_retained(db, scope, execution_id, reason)))
    }
}
