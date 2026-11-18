use super::*;
use crate::{
    blob::{BLOB_CHUNK_BYTES, BlobPrefix, BlobStore},
    work_queue::{WorkKind, WorkLease, WorkQueue},
};
fn due_items(db: &Database, scope: &TenantScope, now: i64) -> Vec<(i64, WorkId)> {
    let mut due = vec![];
    for (id, run) in &db.runs {
        if db.owners.get(id) != Some(scope) {
            continue;
        }
        for timer in run.timers.values() {
            if timer.due_at_ms <= now {
                let work = WorkId {
                    execution_id: id.clone(),
                    kind: WorkKind::Timer {
                        key: timer.key.clone(),
                        generation: timer.generation,
                    },
                };
                // LANG4: a call item, once done, is no longer listed.
                if crate::calls::is_call_key(&timer.key)
                    && db
                        .work_leases
                        .get(&(scope.clone(), work.clone()))
                        .is_some_and(|r| r.acknowledged)
                {
                    continue;
                }
                due.push((timer.due_at_ms, work))
            }
        }
        let activation_at = if run.revision == 0 && db.starts.contains_key(id) {
            match db.catalog.admission(scope, id) {
                Some(admission) if admission.state == crate::catalog::AdmissionState::Admitted => {
                    admission.admitted_at_ms
                }
                Some(_) => None,
                None => db
                    .catalog
                    .get_run(scope, id)
                    .map(|summary| summary.created_at_ms),
            }
        } else {
            None
        };
        if let Some(admitted_at) = activation_at
            && admitted_at <= now
        {
            due.push((
                admitted_at,
                WorkId {
                    execution_id: id.clone(),
                    kind: WorkKind::Activation,
                },
            ));
        }
        for (step_run_id, claim) in &run.claims {
            if !claim.released
                && let Some(deadline) = claim.lease_deadline_at_ms
                && deadline <= now
            {
                due.push((
                    deadline,
                    WorkId {
                        execution_id: id.clone(),
                        kind: WorkKind::ClaimLease {
                            step_run_id: step_run_id.clone(),
                            generation: claim.generation,
                        },
                    },
                ));
            }
        }
        for item in run.outbox.values() {
            if item.at_ms <= now {
                due.push((
                    item.at_ms,
                    WorkId {
                        execution_id: id.clone(),
                        kind: WorkKind::Notification {
                            event_id: item.event_id.clone(),
                        },
                    },
                ))
            }
        }
    }
    due.sort();
    due
}
impl WorkQueue for MemoryExecutionStore {
    async fn claim_due(&self, limit: usize, lease_ms: u64) -> Result<Vec<WorkLease>, StoreError> {
        if lease_ms == 0 {
            return Err(StoreError::InvalidBatch("queue.invalid_lease"));
        }
        let now = self.clock.get();
        let deadline = now
            .checked_add(
                i64::try_from(lease_ms)
                    .map_err(|_| StoreError::InvalidBatch("queue.lease_overflow"))?,
            )
            .ok_or(StoreError::InvalidBatch("queue.lease_overflow"))?;
        self.transaction(|db| {
            let mut claimed = vec![];
            for (_, id) in due_items(db, &self.scope, now) {
                if claimed.len() == limit {
                    break;
                }
                let key = (self.scope.clone(), id.clone());
                let prior = db.work_leases.get(&key);
                if prior.is_some_and(|r| r.acknowledged || r.lease.deadline_at_ms > now) {
                    continue;
                }
                let generation = prior
                    .map_or(0, |r| r.lease.generation)
                    .checked_add(1)
                    .ok_or(StoreError::InvalidBatch("queue.generation_overflow"))?;
                let lease = WorkLease {
                    scope: self.scope.clone(),
                    id,
                    generation,
                    deadline_at_ms: deadline,
                };
                db.work_leases.insert(
                    key,
                    LeaseRecord {
                        lease: lease.clone(),
                        acknowledged: false,
                    },
                );
                claimed.push(lease)
            }
            Ok(claimed)
        })
    }
    async fn ack(&self, id: &WorkId, generation: u64) -> Result<(), StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            if db.owners.get(&id.execution_id) != Some(&self.scope) {
                return Err(StoreError::NotFound);
            }
            // Activation work disappears by being done: an already-started
            // run may still ack its current generation, fenced as usual.
            let activated = match &id.kind {
                WorkKind::Activation => db
                    .runs
                    .get(&id.execution_id)
                    .is_some_and(|run| run.revision > 0),
                WorkKind::ClaimLease { step_run_id, .. } => db
                    .runs
                    .get(&id.execution_id)
                    .and_then(|run| run.claims.get(step_run_id))
                    .is_some_and(|claim| claim.released),
                _ => false,
            };
            if !activated
                && !due_items(db, &self.scope, now)
                    .iter()
                    .any(|(_, item)| item == id)
            {
                return Err(StoreError::InvalidBatch("queue.work_obsolete"));
            }
            let row = db
                .work_leases
                .get_mut(&(self.scope.clone(), id.clone()))
                .ok_or(StoreError::NotFound)?;
            if row.acknowledged
                || row.lease.generation != generation
                || row.lease.deadline_at_ms <= now
            {
                return Err(StoreError::InvalidBatch("queue.stale_lease"));
            }
            row.acknowledged = true;
            Ok(())
        })
    }
}
/// Whether `id` was deleted by retention in this tenant: its uploads are
/// kept until its file expires (PRD §14), readable and deletable, never
/// written again.
fn retained(db: &Database, scope: &TenantScope, id: &ExecutionId) -> bool {
    db.tombstones
        .get(id)
        .is_some_and(|t| &t.scope == scope && t.reason == crate::archives::RETENTION_REASON)
}
fn check_blob(db: &Database, scope: &TenantScope, key: &BlobKey) -> Result<(), StoreError> {
    check_blob_for(db, scope, key, false)
}
fn check_blob_for(
    db: &Database,
    scope: &TenantScope,
    key: &BlobKey,
    write: bool,
) -> Result<(), StoreError> {
    match key {
        BlobKey::Execution { execution_id, name } => {
            if db.owners.get(execution_id) != Some(scope) {
                return Err(StoreError::NotFound);
            }
            if name.is_empty() || name.chars().any(char::is_control) {
                return Err(StoreError::InvalidBatch("blob.invalid_name"));
            }
        }
        // UP1: an upload belongs to its run's prefix (D49).
        BlobKey::Upload { execution_id, .. }
            if db.owners.get(execution_id) != Some(scope)
                && (write || !retained(db, scope, execution_id)) =>
        {
            return Err(StoreError::NotFound);
        }
        // ARC1: a daily archive belongs to a runbook of this tenant (D53).
        BlobKey::Archive { runbook_id, .. }
            if db.catalog.read_runbook(scope, runbook_id).is_none() =>
        {
            return Err(StoreError::NotFound);
        }
        _ => {}
    }
    Ok(())
}
/// Whether `key` lives under one run's prefix (execution blobs and uploads).
pub(super) fn in_run(key: &BlobKey, id: &ExecutionId) -> bool {
    matches!(
        key,
        BlobKey::Execution { execution_id, .. } | BlobKey::Upload { execution_id, .. }
            if execution_id == id
    )
}
impl BlobStore for MemoryExecutionStore {
    async fn put(&self, key: BlobKey, bytes: &[u8]) -> Result<(), StoreError> {
        if matches!(key, BlobKey::Archive { .. }) {
            let hook = self.archive_put_hook.borrow_mut().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        self.transaction(|db| {
            check_blob_for(db, &self.scope, &key, true)?;
            // ARC2: an archive file is addressed by its own digest.
            if let BlobKey::Archive { digest, .. } = &key
                && crate::portable::sha256(bytes) != *digest
            {
                return Err(StoreError::InvalidBatch("blob.digest_mismatch"));
            }
            let scoped = (self.scope.clone(), key);
            if let Some(chunks) = db.blobs.get(&scoped) {
                if chunks.iter().flatten().copied().eq(bytes.iter().copied()) {
                    return Ok(());
                }
                return Err(StoreError::InvalidBatch("blob.immutable_key"));
            }
            if matches!(scoped.1, BlobKey::Archive { .. }) {
                db.archive_put_seq += 1;
                db.archive_versions
                    .insert(scoped.clone(), (db.archive_put_seq, self.clock.get()));
            }
            db.blobs.insert(
                scoped,
                bytes.chunks(BLOB_CHUNK_BYTES).map(<[u8]>::to_vec).collect(),
            );
            Ok(())
        })
    }
    async fn get(&self, key: &BlobKey) -> Result<Vec<u8>, StoreError> {
        let db = self.database.borrow();
        check_blob(&db, &self.scope, key)?;
        db.blobs
            .get(&(self.scope.clone(), key.clone()))
            .map(|chunks| chunks.iter().flatten().copied().collect())
            .ok_or(StoreError::NotFound)
    }
    async fn get_range(
        &self,
        key: &BlobKey,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, StoreError> {
        let db = self.database.borrow();
        check_blob(&db, &self.scope, key)?;
        let chunks = db
            .blobs
            .get(&(self.scope.clone(), key.clone()))
            .ok_or(StoreError::NotFound)?;
        let size: u64 = chunks.iter().map(|c| c.len() as u64).sum();
        if range.start > range.end || range.end > size {
            return Err(StoreError::InvalidBatch("blob.invalid_range"));
        }
        Ok(chunks
            .iter()
            .flatten()
            .skip(range.start as usize)
            .take((range.end - range.start) as usize)
            .copied()
            .collect())
    }
    async fn head(&self, key: &BlobKey) -> Result<crate::blob::BlobHead, StoreError> {
        let db = self.database.borrow();
        check_blob(&db, &self.scope, key)?;
        db.blobs
            .get(&(self.scope.clone(), key.clone()))
            .map(|chunks| crate::blob::BlobHead {
                size: chunks.iter().map(|c| c.len() as u64).sum(),
            })
            .ok_or(StoreError::NotFound)
    }
    async fn delete(&self, key: &BlobKey) -> Result<(), StoreError> {
        self.transaction(|db| {
            check_blob(db, &self.scope, key)?;
            db.blobs
                .remove(&(self.scope.clone(), key.clone()))
                .ok_or(StoreError::NotFound)?;
            db.archive_versions
                .remove(&(self.scope.clone(), key.clone()));
            Ok(())
        })
    }
    async fn list_prefix(&self, prefix: &BlobPrefix) -> Result<Vec<BlobKey>, StoreError> {
        let db = self.database.borrow();
        if let BlobPrefix::Execution(id) = prefix
            && db.owners.get(id) != Some(&self.scope)
        {
            return Err(StoreError::NotFound);
        }
        Ok(db
            .blobs
            .keys()
            .filter(|(scope, key)| {
                scope == &self.scope
                    && match (prefix, key) {
                        (BlobPrefix::Execution(want), key) => in_run(key, want),
                        (BlobPrefix::Shared, BlobKey::Shared { .. }) => true,
                        (
                            BlobPrefix::Archive { runbook_id, day },
                            BlobKey::Archive {
                                runbook_id: r,
                                day: d,
                                ..
                            },
                        ) => r == runbook_id && d == day,
                        _ => false,
                    }
            })
            .map(|(_, key)| key.clone())
            .collect())
    }
}

fn claim_fence(
    db: &Database,
    scope: &TenantScope,
    id: &ExecutionId,
    step_run_id: &runbook_model::StepRunId,
    claim: &runbook_model::ClaimRef,
    owner: &runbook_model::ActorRef,
    now: i64,
) -> Result<ClaimWrite, StoreError> {
    if db.owners.get(id) != Some(scope) {
        return Err(StoreError::NotFound);
    }
    let current = db
        .runs
        .get(id)
        .and_then(|run| run.claims.get(step_run_id))
        .ok_or(StoreError::NotFound)?;
    if current.released
        || current.claim_id != claim.id
        || current.generation != claim.generation.get()
        || &current.owner != owner
        || current
            .lease_deadline_at_ms
            .is_some_and(|deadline| now >= deadline)
    {
        return Err(StoreError::InvalidBatch("queue.stale_lease"));
    }
    Ok(current.clone())
}
impl crate::claims::ClaimStore for MemoryExecutionStore {
    async fn heartbeat(
        &self,
        id: &ExecutionId,
        step_run_id: &runbook_model::StepRunId,
        claim: &runbook_model::ClaimRef,
        owner: &runbook_model::ActorRef,
        replay: Option<&crate::actor::Replay>,
    ) -> Result<Option<i64>, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            if db.owners.get(id) != Some(&self.scope) {
                return Err(StoreError::NotFound);
            }
            let operation = format!("heartbeat:{}", step_run_id);
            if let Some(replay) = replay
                && let Some(value) = replay.lookup(
                    &operation,
                    actor_replay::lookup(db, id, &replay.prefix(&operation)?, now).iter(),
                    now,
                )?
            {
                return Ok(value);
            }
            let mut current = claim_fence(db, &self.scope, id, step_run_id, claim, owner, now)?;
            if current.lease_deadline_at_ms.is_some() {
                current.lease_deadline_at_ms = Some(now + crate::claims::AGENT_LEASE_MS);
            }
            let deadline = current.lease_deadline_at_ms;
            db.runs
                .get_mut(id)
                .expect("fenced run exists")
                .claims
                .insert(step_run_id.clone(), current);
            if let Some(replay) = replay {
                let row = replay.record(&operation, id, now, deadline)?;
                actor_replay::write(db, row, now)?;
                self.checkpoint(WriteStage::Idempotency)?;
            }
            Ok(deadline)
        })
    }
    async fn append_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &runbook_model::StepRunId,
        claim: &runbook_model::ClaimRef,
        owner: &runbook_model::ActorRef,
        lines: Vec<String>,
        replay: Option<&crate::actor::Replay>,
    ) -> Result<u64, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            if db.owners.get(id) != Some(&self.scope) {
                return Err(StoreError::NotFound);
            }
            let operation = format!("append_logs:{}", step_run_id);
            if let Some(replay) = replay
                && let Some(value) = replay.lookup(
                    &operation,
                    actor_replay::lookup(db, id, &replay.prefix(&operation)?, now).iter(),
                    now,
                )?
            {
                return Ok(value);
            }
            claim_fence(db, &self.scope, id, step_run_id, claim, owner, now)?;
            let log = db
                .claim_logs
                .entry((self.scope.clone(), id.clone(), step_run_id.clone()))
                .or_default();
            let total = log
                .iter()
                .chain(&lines)
                .map(|line| crate::claims::log_entry_bytes(line))
                .sum::<u64>();
            if total > crate::claims::MAX_STEP_LOG_BYTES {
                return Err(StoreError::InvalidBatch("step.logs_too_large"));
            }
            log.extend(lines);
            let count = log.len() as u64;
            if let Some(replay) = replay {
                let row = replay.record(&operation, id, now, count)?;
                actor_replay::write(db, row, now)?;
                self.checkpoint(WriteStage::Idempotency)?;
            }
            Ok(count)
        })
    }
    async fn claim_record(
        &self,
        id: &ExecutionId,
        step_run_id: &runbook_model::StepRunId,
    ) -> Result<Option<ClaimWrite>, StoreError> {
        let db = self.database.borrow();
        if db.owners.get(id) != Some(&self.scope) {
            return Ok(None);
        }
        Ok(db
            .runs
            .get(id)
            .and_then(|run| run.claims.get(step_run_id))
            .cloned())
    }
    async fn read_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &runbook_model::StepRunId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        let db = self.database.borrow();
        if db.owners.get(id) != Some(&self.scope) {
            return Err(StoreError::NotFound);
        }
        let key = (self.scope.clone(), id.clone(), step_run_id.clone());
        Ok(db
            .claim_logs
            .get(&key)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, line)| (index as u64 + 1, line.clone()))
            .filter(|(seq, _)| *seq > after)
            .take(limit)
            .collect())
    }
}
