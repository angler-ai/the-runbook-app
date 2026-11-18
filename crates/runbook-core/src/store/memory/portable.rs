//! D3 portable archive rows for the memory execution store. Import stages
//! a whole-database clone; the caller installs it only after every other
//! memory adapter staged successfully, so a failure leaves nothing behind.
use super::*;
use crate::portable::{BlobRow, ClaimLog, PortableError, RunUnit, StartReplayRow, StoreRows};

pub(crate) struct StagedStore(Database);

fn has_rows(db: &Database, scope: &TenantScope) -> bool {
    db.catalog.portable_has_rows(scope)
        || db.owners.values().any(|owner| owner == scope)
        || db.tombstones.values().any(|t| &t.scope == scope)
        || db.blobs.keys().any(|(owner, _)| owner == scope)
        || db.work_leases.keys().any(|(owner, _)| owner == scope)
        || db.start_replays.keys().any(|(owner, _)| owner == scope)
        || db.claim_logs.keys().any(|(owner, _, _)| owner == scope)
        || db.project_outbox.keys().any(|(owner, _)| owner == scope)
        || db.memory.holds(scope)
}

impl MemoryExecutionStore {
    /// A store with no runs, for hosts and imports that start empty.
    pub fn empty(scope: TenantScope, now_ms: i64) -> Self {
        Self {
            database: Default::default(),
            fail: Default::default(),
            scope,
            clock: Rc::new(Cell::new(now_ms)),
            archive_put_hook: Default::default(),
            quota: Default::default(),
            run_budget: crate::run_budget::DEFAULT_MAX_RUN_BYTES,
        }
    }
    pub(crate) fn portable_scope(&self) -> &TenantScope {
        &self.scope
    }
    pub(crate) fn portable_rows(&self) -> Result<StoreRows, StoreError> {
        let db = self.database.borrow();
        let scope = &self.scope;
        let mut rows = db.catalog.portable_rows(scope);
        for (id, owner) in &db.owners {
            if owner != scope || !db.runs.contains_key(id) {
                continue;
            }
            rows.runs.push(RunUnit {
                slice: super::retention::slice(&db, scope, id)?,
                start: db.starts.get(id).cloned(),
                start_replays: db
                    .start_replays
                    .iter()
                    .filter(|((owner, _), (replay, _))| {
                        owner == scope && &replay.outcome.execution_id == id
                    })
                    .map(|(_, (replay, created_at_ms))| StartReplayRow {
                        replay: replay.clone(),
                        created_at_ms: *created_at_ms,
                    })
                    .collect(),
                claim_logs: db
                    .claim_logs
                    .iter()
                    .filter(|((owner, execution_id, _), _)| owner == scope && execution_id == id)
                    .map(|((_, _, step_run_id), lines)| ClaimLog {
                        step_run_id: step_run_id.clone(),
                        lines: lines.clone(),
                    })
                    .collect(),
            });
        }
        rows.tombstones = db
            .tombstones
            .values()
            .filter(|t| &t.scope == scope)
            .cloned()
            .collect();
        rows.blobs = db
            .blobs
            .iter()
            .filter(|((owner, _), _)| owner == scope)
            .map(|((_, key), chunks)| BlobRow {
                key: key.clone(),
                bytes: chunks.concat(),
            })
            .collect();
        rows.memory = db.memory.export(scope);
        Ok(rows)
    }
    /// Stage the archive's rows into a copy of the database.
    pub(crate) fn portable_stage(&self, rows: &StoreRows) -> Result<StagedStore, PortableError> {
        let scope = &self.scope;
        let mut db = self.database.borrow().clone();
        if has_rows(&db, scope) {
            return Err(PortableError::TargetNotEmpty);
        }
        let conflict = PortableError::Conflict("import.id_conflict");
        // Step run IDs are global (step URLs): none may exist already.
        if rows
            .runs
            .iter()
            .flat_map(|u| &u.slice.run.projection.steps)
            .any(|step| {
                db.runs.values().any(|run| {
                    run.projection
                        .steps
                        .iter()
                        .any(|s| s.step_run_id == step.step_run_id)
                })
            })
        {
            return Err(conflict);
        }
        db.catalog.portable_import(scope, rows)?;
        for unit in &rows.runs {
            let slice = &unit.slice;
            let id = &slice.run.projection.execution_id;
            if db.runs.contains_key(id)
                || db.owners.contains_key(id)
                || db.tombstones.contains_key(id)
            {
                return Err(conflict);
            }
            db.owners.insert(id.clone(), scope.clone());
            db.runs.insert(id.clone(), slice.run.clone());
            for record in &slice.actor_replays {
                let (prefix, at) = crate::actor::replay_record_parts(record)?
                    .ok_or(StoreError::InvalidBatch("replay.invalid_record"))?;
                db.actor_replays
                    .insert((id.clone(), prefix.to_owned(), at), record.clone());
            }
            for work in &slice.work {
                db.work_leases.insert(
                    (scope.clone(), work.lease.id.clone()),
                    LeaseRecord {
                        lease: work.lease.clone(),
                        acknowledged: work.acknowledged,
                    },
                );
            }
            if let Some(start) = &unit.start {
                db.starts.insert(id.clone(), start.clone());
            }
            for row in &unit.start_replays {
                db.start_replays.insert(
                    (scope.clone(), row.replay.key.clone()),
                    (row.replay.clone(), row.created_at_ms),
                );
            }
            for log in &unit.claim_logs {
                db.claim_logs.insert(
                    (scope.clone(), id.clone(), log.step_run_id.clone()),
                    log.lines.clone(),
                );
            }
        }
        for tombstone in &rows.tombstones {
            if db.tombstones.contains_key(&tombstone.execution_id)
                || db.owners.contains_key(&tombstone.execution_id)
            {
                return Err(conflict);
            }
            db.tombstones
                .insert(tombstone.execution_id.clone(), tombstone.clone());
        }
        for blob in &rows.blobs {
            db.blobs.insert(
                (scope.clone(), blob.key.clone()),
                blob.bytes
                    .chunks(crate::blob::BLOB_CHUNK_BYTES)
                    .map(<[u8]>::to_vec)
                    .collect(),
            );
        }
        db.memory.import(&rows.memory).map_err(|e| match e {
            StoreError::InvalidBatch("import.id_conflict") => conflict,
            other => other.into(),
        })?;
        Ok(StagedStore(db))
    }
    pub(crate) fn portable_install(&self, staged: StagedStore) {
        *self.database.borrow_mut() = staged.0;
    }
}
