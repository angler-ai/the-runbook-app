//! A single synchronous storage transaction; deliberately Rc-backed and non-Send.
use super::*;
use crate::blob::BlobKey;
use crate::work_queue::{LeaseRecord, WorkId};
mod actor_replay;
mod archives;
mod credentials;
mod io;
mod memory_entries;
mod pending;
mod portable;
mod retention;
mod uploads;
use crate::catalog::{
    Admission, AdmitRequest, Assignment, CatalogStore, DraftRevision, RunSummary, TenantScope,
    data::CatalogData,
};
use crate::session::{ExecutionSession, SessionStore};
use runbook_model::ExecutionId;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
#[derive(Clone, Default)]
struct Database {
    runs: BTreeMap<ExecutionId, StoredExecution>,
    actor_replays: BTreeMap<(ExecutionId, String, i64), IdempotencyRecord>,
    tombstones: BTreeMap<ExecutionId, crate::retention::Tombstone>,
    owners: BTreeMap<ExecutionId, TenantScope>,
    catalog: CatalogData,
    work_leases: BTreeMap<(TenantScope, WorkId), LeaseRecord>,
    starts: BTreeMap<ExecutionId, crate::start::StartRecord>,
    credential_families:
        BTreeMap<String, (TenantScope, crate::execution_credentials::CredentialFamily)>,
    exec_credentials:
        BTreeMap<runbook_model::TokenId, crate::execution_credentials::ExecutionCredentialRecord>,
    claim_logs: BTreeMap<(TenantScope, ExecutionId, runbook_model::StepRunId), Vec<String>>,
    start_replays: BTreeMap<(TenantScope, String), (crate::start::StartReplay, i64)>,
    blobs: BTreeMap<(TenantScope, BlobKey), Vec<Vec<u8>>>,
    /// C1 project-level webhook outbox (`runbook.published`).
    project_outbox: BTreeMap<(TenantScope, String), crate::webhooks::ProjectEvent>,
    /// MEM1 runbook memory (entries with their revisions).
    memory: crate::memory_entries::data::MemoryData,
    /// UP1 evidence upload slots, by ID (tenant-filtered on every read).
    evidence: BTreeMap<runbook_model::EvidenceId, crate::uploads::EvidenceRecord>,
    /// ARC1 daily archive index: one row per (tenant, runbook, day) file.
    archives:
        BTreeMap<(TenantScope, runbook_model::RunbookId, i64), crate::archives::ArchiveRecord>,
    /// ARC2 summary rows: where each archived run lives, kept after
    /// retention deletes its live detail.
    archived_runs: BTreeMap<ExecutionId, (TenantScope, crate::archives::ArchivedRun)>,
    /// ARC2 failure backoff per day: (attempts, next attempt at).
    archive_backoff: BTreeMap<(TenantScope, runbook_model::RunbookId, i64), (u32, i64)>,
    /// ARC2 file membership: every run a file lists, until the file goes.
    archive_members: BTreeMap<ExecutionId, (TenantScope, runbook_model::RunbookId, i64)>,
    /// ARC2 when each stored archive version was put, as (put sequence,
    /// store time): the store sweeps only versions put before the named one,
    /// or older than the grace period.
    archive_versions: BTreeMap<(TenantScope, BlobKey), (u64, i64)>,
    archive_put_seq: u64,
    /// ARC2 evidence of runs retention deleted: still counted toward
    /// storage until the file expires or a single-run deletion (PRD §14).
    retained_evidence: BTreeMap<runbook_model::EvidenceId, crate::uploads::EvidenceRecord>,
    /// CF6 starts counted per (organization, `YYYY-MM`).
    usage: BTreeMap<(runbook_model::OrganizationId, String), u64>,
    /// CF6 which month each counted start was counted in (for its refund).
    quota_reservations: BTreeMap<ExecutionId, (runbook_model::OrganizationId, String)>,
}
#[derive(Clone)]
pub struct MemoryExecutionStore {
    database: Rc<RefCell<Database>>,
    fail: Rc<Cell<Option<WriteStage>>>,
    scope: TenantScope,
    clock: Rc<Cell<i64>>,
    /// Test seam (ARC2): runs once, before the next archive-file put.
    archive_put_hook: ArchivePutHook,
    /// CF6 quota limits (configuration; none by default).
    quota: crate::quotas::QuotaLimits,
    /// CF13 review round 1: the run budget ([`crate::run_budget`]).
    run_budget: u64,
}
type ArchivePutHook = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;
impl MemoryExecutionStore {
    /// Test seam: run `hook` once, just before the next put of an archive
    /// file, outside any transaction (so it may call this store). Lets a
    /// test interleave a deletion or a second writer inside a rewrite.
    pub fn before_next_archive_put(&self, hook: impl FnOnce() + 'static) {
        *self.archive_put_hook.borrow_mut() = Some(Box::new(hook));
    }
    pub fn new(scope: TenantScope, projection: ExecutionProjection, now_ms: i64) -> Self {
        let store = Self {
            database: Default::default(),
            fail: Default::default(),
            scope: scope.clone(),
            clock: Rc::new(Cell::new(now_ms)),
            archive_put_hook: Default::default(),
            quota: Default::default(),
            run_budget: crate::run_budget::DEFAULT_MAX_RUN_BYTES,
        };
        store
            .database
            .borrow_mut()
            .catalog
            .claim_execution(&scope, &projection.execution_id)
            .expect("fresh catalog");
        store
            .database
            .borrow_mut()
            .owners
            .insert(projection.execution_id.clone(), scope);
        store.database.borrow_mut().runs.insert(
            projection.execution_id.clone(),
            StoredExecution {
                revision: 0,
                projection,
                events: vec![],
                timers: Default::default(),
                timer_history: Default::default(),
                outbox: Default::default(),
                idempotency: Default::default(),
                claims: Default::default(),
                derived_outbox: Default::default(),
            },
        );
        store
    }
    /// Retry only a recorded outbox entry, never a caller-invented derived row.
    pub fn redeliver_derived(&self, id: &ExecutionId, sequence: u64) -> Result<(), StoreError> {
        self.transaction(|db| {
            if db.owners.get(id) != Some(&self.scope) {
                return Err(StoreError::NotFound);
            }
            let update = db
                .runs
                .get(id)
                .and_then(|r| r.derived_outbox.get(&sequence))
                .cloned()
                .ok_or(StoreError::NotFound)?;
            memory_entries::apply_derived_memory(&mut db.memory, &self.scope, &update)?;
            db.catalog
                .apply_derived(&self.scope, update, self.clock.get())
        })
    }
    pub fn scoped(&self, scope: TenantScope) -> Self {
        Self {
            scope,
            ..self.clone()
        }
    }
    /// CF6: the same storage with `limits` in force.
    pub fn with_quota(&self, limits: crate::quotas::QuotaLimits) -> Self {
        Self {
            quota: limits,
            ..self.clone()
        }
    }
    /// CF13 review round 1: the same storage with run budget `bytes`.
    pub fn with_run_budget(&self, bytes: u64) -> Self {
        Self {
            run_budget: bytes,
            ..self.clone()
        }
    }
    pub fn backend_time_ms(&self) -> i64 {
        self.clock.get()
    }
    pub fn set_time(&self, now_ms: i64) {
        self.clock.set(now_ms)
    }
    pub fn fail_after_events(&self) {
        self.fail.set(Some(WriteStage::Events));
    }
    pub fn fail_after(&self, stage: WriteStage) {
        self.fail.set(Some(stage));
    }
    fn checkpoint(&self, stage: WriteStage) -> Result<(), StoreError> {
        if self.fail.get() == Some(stage) {
            self.fail.set(None);
            Err(StoreError::Backend("injected"))
        } else {
            Ok(())
        }
    }
    /// No await inside this closure, matching a synchronous DO transaction.
    fn transaction<T>(
        &self,
        action: impl FnOnce(&mut Database) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut committed = self.database.borrow_mut();
        let mut staged = committed.clone();
        let result = action(&mut staged)?;
        *committed = staged;
        Ok(result)
    }
}
impl ExecutionStore for MemoryExecutionStore {
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        let db = self.database.borrow();
        if db.owners.get(id) != Some(&self.scope) {
            return Err(StoreError::NotFound);
        }
        db.runs.get(id).cloned().ok_or(StoreError::NotFound)
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| self.commit_in(db, batch, now))
    }
}

impl MemoryExecutionStore {
    /// One execution commit inside an open transaction (LANG3 applies
    /// prepared cancellations inside the start transaction), at transaction
    /// time `now`: the store clock, or a session's begin time (CF2 fix 2:
    /// the clock may move between begin and commit, as on Postgres).
    fn commit_in(
        &self,
        db: &mut Database,
        batch: ExecutionCommit,
        now: i64,
    ) -> Result<CommitResult, StoreError> {
        if db.tombstones.contains_key(&batch.execution_id) {
            return Err(StoreError::NotFound);
        }
        if db
            .owners
            .get(&batch.execution_id)
            .is_some_and(|scope| scope != &self.scope)
        {
            return Err(StoreError::NotFound);
        }
        let previous = db.runs.get(&batch.execution_id);
        if previous.map_or(0, |run| run.revision) != batch.expected_sequence {
            return Ok(CommitResult::Conflict);
        }
        let sequence = batch.validate()?;
        if batch.events.is_empty() && previous.is_none_or(|r| r.projection != batch.projection) {
            return Err(StoreError::InvalidBatch("store.replay_projection_changed"));
        }
        if let Some(previous) = previous {
            if previous.projection.runbook_id != batch.projection.runbook_id
                || previous.projection.version_id != batch.projection.version_id
                || previous.projection.compiled_hash != batch.projection.compiled_hash
            {
                return Err(StoreError::InvalidBatch("store.pinned_version_changed"));
            }
            if batch
                .outbox
                .iter()
                .any(|intent| previous.outbox.contains_key(&intent.event_id))
            {
                return Err(StoreError::InvalidBatch("store.duplicate_outbox"));
            }
        }
        for change in &batch.timers {
            if let TimerChange::Upsert(timer) = change
                && let Some(prior) = previous.and_then(|r| {
                    r.timer_history
                        .get(&timer.key)
                        .or_else(|| r.timers.get(&timer.key))
                })
                && (timer.generation < prior.generation
                    || (timer.generation == prior.generation
                        && previous.and_then(|r| r.timers.get(&timer.key)) != Some(timer)))
            {
                return Err(StoreError::InvalidBatch("store.timer_generation_reused"));
            }
        }
        for claim in &batch.claims {
            let generation = previous
                .and_then(|r| r.claims.get(&claim.step_run_id))
                .map_or(0, |r| r.generation);
            if generation != claim.expected_generation {
                return Ok(CommitResult::Conflict);
            }
        }
        if batch
            .idempotency
            .iter()
            .any(|r| previous.is_some_and(|p| p.idempotency.contains_key(&r.key)))
        {
            return Err(StoreError::InvalidBatch("store.idempotency_conflict"));
        }
        for write in &batch.credentials {
            crate::store::memory::credentials::precheck(db, &self.scope, write)?;
        }
        db.catalog
            .claim_execution(&self.scope, &batch.execution_id)?;
        db.owners
            .insert(batch.execution_id.clone(), self.scope.clone());
        for write in batch.credentials.clone() {
            crate::store::memory::credentials::apply(db, write);
        }
        self.checkpoint(WriteStage::Credentials)?;
        for row in &batch.idempotency {
            if crate::actor::replay_record_parts(row)?.is_some() {
                actor_replay::write(db, row.clone(), now)?;
            }
        }
        let run = db
            .runs
            .entry(batch.execution_id.clone())
            .or_insert_with(|| StoredExecution {
                revision: 0,
                projection: batch.projection.clone(),
                events: vec![],
                timers: Default::default(),
                timer_history: Default::default(),
                outbox: Default::default(),
                idempotency: Default::default(),
                claims: Default::default(),
                derived_outbox: Default::default(),
            });
        let appended = !batch.events.is_empty();
        run.events.extend(batch.events);
        self.checkpoint(WriteStage::Events)?;
        run.projection = batch.projection;
        self.checkpoint(WriteStage::Projection)?;
        for change in batch.timers {
            match change {
                TimerChange::Upsert(timer) => {
                    run.timer_history.insert(timer.key.clone(), timer.clone());
                    run.timers.insert(timer.key.clone(), timer);
                }
                TimerChange::Delete { key, .. } => {
                    if let Some(prior) = run.timers.get(&key) {
                        run.timer_history.insert(key.clone(), prior.clone());
                    }
                    run.timers.remove(&key);
                }
            }
        }
        self.checkpoint(WriteStage::Timers)?;
        for intent in batch.outbox {
            run.outbox.insert(intent.event_id.clone(), intent);
        }
        self.checkpoint(WriteStage::Outbox)?;
        for row in batch.idempotency {
            if crate::actor::replay_record_parts(&row)?.is_none() {
                run.idempotency.insert(row.key.clone(), row);
            }
        }
        self.checkpoint(WriteStage::Idempotency)?;
        for row in batch.claims {
            run.claims.insert(row.step_run_id.clone(), row);
        }
        self.checkpoint(WriteStage::Claims)?;
        for update in batch.derived_outbox {
            memory_entries::apply_derived_memory(&mut db.memory, &self.scope, &update)?;
            db.catalog
                .apply_derived(&self.scope, update.clone(), self.clock.get())?;
            run.derived_outbox.insert(update.summary.sequence, update);
        }
        self.checkpoint(WriteStage::DerivedRows)?;
        let closed_at = matches!(
            run.projection.status,
            runbook_model::ExecutionStatus::Succeeded
                | runbook_model::ExecutionStatus::Failed
                | runbook_model::ExecutionStatus::Cancelled
        )
        .then(|| run.events.last().map_or(0, |event| event.at_ms));
        run.revision = sequence;
        // ARC1: new events on a run already in a daily file (an amendment)
        // make that file stale in this same transaction.
        if appended {
            archives::mark_run_stale(db, &batch.execution_id);
        }
        if let Some(closed_at) = closed_at {
            crate::store::memory::credentials::revoke_all(db, &batch.execution_id, closed_at);
        }
        Ok(CommitResult::Committed(sequence))
    }
}

impl CatalogStore for MemoryExecutionStore {
    async fn read_draft(&self, key: &str) -> Result<Option<DraftRevision>, StoreError> {
        Ok(self.database.borrow().catalog.read_draft(&self.scope, key))
    }
    async fn replace_draft(
        &self,
        key: &str,
        expected: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        self.transaction(|db| db.catalog.replace_draft(&self.scope, key, expected, source))
    }
    async fn list_runs(&self) -> Result<Vec<RunSummary>, StoreError> {
        Ok(self.database.borrow().catalog.list_runs(&self.scope))
    }
    async fn get_run(&self, id: &ExecutionId) -> Result<Option<RunSummary>, StoreError> {
        Ok(self.database.borrow().catalog.get_run(&self.scope, id))
    }
    async fn list_assignments(&self) -> Result<Vec<Assignment>, StoreError> {
        Ok(self.database.borrow().catalog.list_assignments(&self.scope))
    }
    async fn admission_pool(&self, key: &str) -> Result<crate::concurrency::PoolView, StoreError> {
        Ok(self
            .database
            .borrow()
            .catalog
            .admission_pool(&self.scope, key))
    }
    async fn admit(&self, request: AdmitRequest) -> Result<Admission, StoreError> {
        self.transaction(|db| {
            if db.tombstones.contains_key(&request.execution_id) {
                return Err(StoreError::NotFound);
            }
            db.catalog.admit(&self.scope, request, self.clock.get())
        })
    }
    async fn admission(&self, id: &ExecutionId) -> Result<Option<Admission>, StoreError> {
        Ok(self.database.borrow().catalog.admission(&self.scope, id))
    }
    async fn create_runbook(
        &self,
        record: crate::catalog::RunbookRecord,
        source: String,
    ) -> Result<runbook_model::DraftView, StoreError> {
        self.transaction(|db| db.catalog.create_runbook(&self.scope, record, source))
    }
    async fn read_runbook(
        &self,
        id: &runbook_model::RunbookId,
    ) -> Result<Option<crate::catalog::RunbookRecord>, StoreError> {
        Ok(self.database.borrow().catalog.read_runbook(&self.scope, id))
    }
    async fn get_draft(
        &self,
        id: &runbook_model::RunbookId,
    ) -> Result<Option<runbook_model::DraftView>, StoreError> {
        Ok(self.database.borrow().catalog.get_draft(&self.scope, id))
    }
    async fn save_draft(
        &self,
        id: &runbook_model::RunbookId,
        expected_revision: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        self.transaction(|db| {
            db.catalog
                .save_draft(&self.scope, id, expected_revision, source)
        })
    }
    async fn publish_revision(
        &self,
        id: &runbook_model::RunbookId,
        expected_revision: u64,
        prepared: crate::publish::PreparedPublication,
    ) -> Result<runbook_model::PublishedVersionView, StoreError> {
        let now = self.clock.get();
        let fresh = prepared.version_id.clone();
        self.transaction(|db| {
            let view = db
                .catalog
                .publish_revision(&self.scope, id, expected_revision, prepared)?;
            // A semantic duplicate returns the existing version: nothing new
            // was published, so no event.
            if view.runbook_version_id == fresh {
                let slug = db
                    .catalog
                    .read_runbook(&self.scope, id)
                    .ok_or(StoreError::NotFound)?
                    .slug;
                let event = crate::webhooks::published_event(id, &slug, &view, now);
                db.project_outbox
                    .insert((self.scope.clone(), event.event_id.clone()), event);
            }
            Ok(view)
        })
    }
    async fn create_preview(
        &self,
        id: &runbook_model::RunbookId,
        prepared: crate::publish::PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<runbook_model::VersionView, StoreError> {
        self.transaction(|db| {
            db.catalog
                .create_preview(&self.scope, id, prepared, branch, commit_sha)
        })
    }
    async fn resolve_version(
        &self,
        id: &runbook_model::RunbookId,
        selector: Option<runbook_model::VersionSelector>,
    ) -> Result<runbook_model::VersionView, StoreError> {
        self.database
            .borrow()
            .catalog
            .resolve_version(&self.scope, id, selector)
    }
    async fn version_record(
        &self,
        runbook_id: &runbook_model::RunbookId,
        version_id: &runbook_model::VersionId,
    ) -> Result<Option<crate::catalog::VersionRecord>, StoreError> {
        Ok(self
            .database
            .borrow()
            .catalog
            .version_record(&self.scope, runbook_id, version_id))
    }
    async fn list_versions(
        &self,
        id: &runbook_model::RunbookId,
    ) -> Result<Vec<runbook_model::PublishedVersionView>, StoreError> {
        self.database
            .borrow()
            .catalog
            .list_versions(&self.scope, id)
    }
    async fn list_previews(
        &self,
        id: &runbook_model::RunbookId,
    ) -> Result<Vec<runbook_model::VersionView>, StoreError> {
        self.database
            .borrow()
            .catalog
            .list_previews(&self.scope, id)
    }
    async fn record_skill_version(
        &self,
        version: crate::catalog::SkillVersion,
    ) -> Result<crate::catalog::SkillVersion, StoreError> {
        self.transaction(|db| Ok(db.catalog.record_skill_version(&self.scope, version)))
    }
    async fn skill_version(
        &self,
        hash: &[u8; 32],
    ) -> Result<Option<crate::catalog::SkillVersion>, StoreError> {
        Ok(self
            .database
            .borrow()
            .catalog
            .skill_version(&self.scope, hash))
    }
    async fn latest_skills(&self) -> Result<Vec<crate::catalog::SkillVersion>, StoreError> {
        Ok(self.database.borrow().catalog.latest_skills(&self.scope))
    }
    async fn execution_project(
        &self,
        org_id: &runbook_model::OrganizationId,
        id: &ExecutionId,
    ) -> Result<Option<runbook_model::ProjectId>, StoreError> {
        Ok(self
            .database
            .borrow()
            .owners
            .get(id)
            .filter(|scope| &scope.org_id == org_id)
            .map(|scope| scope.project_id.clone()))
    }
    async fn tombstone_project(
        &self,
        org_id: &runbook_model::OrganizationId,
        id: &ExecutionId,
    ) -> Result<Option<runbook_model::ProjectId>, StoreError> {
        Ok(self
            .database
            .borrow()
            .tombstones
            .get(id)
            .filter(|t| &t.scope.org_id == org_id)
            .map(|t| t.scope.project_id.clone()))
    }
    async fn search_runs(
        &self,
        filters: crate::search::RunFilters,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<crate::search::RunPage, StoreError> {
        self.database
            .borrow()
            .catalog
            .search_runs(&self.scope, filters, cursor, limit)
    }
    async fn versions_pinning_skill(
        &self,
        hash: &[u8; 32],
    ) -> Result<Vec<(runbook_model::RunbookId, crate::catalog::VersionRecord)>, StoreError> {
        Ok(self
            .database
            .borrow()
            .catalog
            .versions_pinning_skill(&self.scope, hash))
    }
}
/// Immutable read snapshot with store-supplied time; commit still CASes live state.
pub struct MemorySession {
    store: MemoryExecutionStore,
    snapshot: StoredExecution,
    at_ms: i64,
    consumed: Cell<bool>,
}
impl ExecutionSession for MemorySession {
    fn transaction_time_ms(&self) -> i64 {
        self.at_ms
    }
    async fn actor_replay(&self, prefix: &str) -> Result<Option<IdempotencyRecord>, StoreError> {
        if self.consumed.get() {
            return Err(StoreError::InvalidBatch("store.session_closed"));
        }
        let db = self.store.database.borrow();
        let id = &self.snapshot.projection.execution_id;
        if db.owners.get(id) != Some(&self.store.scope) {
            return Err(StoreError::NotFound);
        }
        Ok(actor_replay::lookup(&db, id, prefix, self.at_ms))
    }
}
impl SessionStore for MemoryExecutionStore {
    type Session = MemorySession;
    fn run_budget(&self) -> u64 {
        self.run_budget
    }
    async fn begin(&self, id: &ExecutionId) -> Result<Self::Session, StoreError> {
        Ok(MemorySession {
            store: self.clone(),
            snapshot: self.load(id).await?,
            at_ms: self.clock.get(),
            consumed: Cell::new(false),
        })
    }
}
impl ExecutionStore for MemorySession {
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        if id != &self.snapshot.projection.execution_id {
            return Err(StoreError::NotFound);
        }
        if self.consumed.get() {
            return Err(StoreError::InvalidBatch("store.session_closed"));
        }
        Ok(self.snapshot.clone())
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        if self.consumed.replace(true) {
            return Err(StoreError::InvalidBatch("store.session_closed"));
        }
        if batch.execution_id != self.snapshot.projection.execution_id {
            return Err(StoreError::NotFound);
        }
        if batch.expected_sequence != self.snapshot.revision {
            return Ok(CommitResult::Conflict);
        }
        batch.check_transaction_time(self.at_ms, self.snapshot.projection.completed_at_ms)?;
        let at_ms = self.at_ms;
        self.store
            .transaction(|db| self.store.commit_in(db, batch, at_ms))
    }
}

impl crate::start::StartStore for MemoryExecutionStore {
    async fn commit_start(
        &self,
        commit: crate::start::StartCommit,
    ) -> Result<crate::start::StartOutcome, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            if let Some(replay) = &commit.replay {
                let key = (self.scope.clone(), replay.key.clone());
                if db
                    .start_replays
                    .get(&key)
                    .is_some_and(|(_, created)| now - *created >= crate::start::REPLAY_RETENTION_MS)
                {
                    db.start_replays.remove(&key);
                }
                if let Some((saved, _)) = db.start_replays.get(&key) {
                    if saved.principal != replay.principal
                        || saved.request_hash != replay.request_hash
                    {
                        return Err(StoreError::InvalidBatch("request.idempotency_key_reused"));
                    }
                    return Ok(saved.outcome.clone());
                }
            }
            let id = &commit.projection.execution_id;
            if db.tombstones.contains_key(id) {
                return Err(StoreError::NotFound);
            }
            if db.owners.get(id).is_some_and(|scope| scope != &self.scope) {
                return Err(StoreError::NotFound);
            }
            if db.runs.contains_key(id) {
                return Err(StoreError::InvalidBatch("store.execution_exists"));
            }
            // CF6: count the start in its month, refusing it past the limit
            // (the transaction rolls the count back if anything later fails).
            quota::reserve(db, &self.quota, &self.scope, id, now)?;
            db.catalog.claim_execution(&self.scope, id)?;
            db.owners.insert(id.clone(), self.scope.clone());
            db.runs.insert(
                id.clone(),
                StoredExecution {
                    revision: 0,
                    projection: commit.projection.clone(),
                    events: vec![],
                    timers: Default::default(),
                    timer_history: Default::default(),
                    outbox: Default::default(),
                    idempotency: Default::default(),
                    claims: Default::default(),
                    derived_outbox: Default::default(),
                },
            );
            db.starts.insert(id.clone(), commit.record.clone());
            let admission = match &commit.admission {
                Some(request) => {
                    // LANG3: the core resolves the conflict policy against
                    // the pool. A preempting start releases the evicted
                    // waiters, applies the prepared cancellations (a holder
                    // keeps its slot until it closes) and is admitted first
                    // in line, all in this transaction.
                    let view = db.catalog.admission_pool(&self.scope, &request.key);
                    let resolution = crate::concurrency::resolve(
                        commit.on_conflict.as_ref(),
                        request.limit,
                        &view,
                    )?;
                    let first =
                        matches!(resolution, crate::concurrency::Resolution::Preempt { .. });
                    if let crate::concurrency::Resolution::Preempt { waiters } = &resolution {
                        for waiter in waiters {
                            db.catalog.release_admission(&self.scope, waiter, now);
                            // An evicted waiter was cancelled while queued.
                            quota::refund(db, waiter);
                        }
                        if let Some(crate::concurrency::OnConflict::CancelPrevious {
                            commits,
                            ..
                        }) = &commit.on_conflict
                        {
                            for batch in commits {
                                if self.commit_in(db, batch.clone(), now)? == CommitResult::Conflict
                                {
                                    return Err(StoreError::InvalidBatch(
                                        crate::concurrency::STALE_CODE,
                                    ));
                                }
                            }
                        }
                    }
                    Some(
                        db.catalog
                            .admit_pooled_at(&self.scope, request.clone(), now, first)?,
                    )
                }
                None => None,
            };
            if let Some(intent) =
                crate::start::queued_outbox(id, &commit.record, admission.as_ref(), now)
                && let Some(run) = db.runs.get_mut(id)
            {
                run.outbox.insert(intent.event_id.clone(), intent);
            }
            let mut derived = commit.derived.clone();
            derived.summary.created_at_ms = now;
            db.catalog.apply_derived(&self.scope, derived, now)?;
            let outcome = crate::start::StartOutcome {
                execution_id: id.clone(),
                runbook_version_id: commit.record.version_id.clone(),
                runbook_version: commit.record.version,
                admission,
            };
            if let Some(mut replay) = commit.replay.clone() {
                replay.outcome = outcome.clone();
                db.start_replays
                    .insert((self.scope.clone(), replay.key.clone()), (replay, now));
            }
            Ok(outcome)
        })
    }
    async fn start_record(
        &self,
        id: &ExecutionId,
    ) -> Result<Option<crate::start::StartRecord>, StoreError> {
        let db = self.database.borrow();
        if db.owners.get(id) != Some(&self.scope) {
            return Ok(None);
        }
        Ok(db.starts.get(id).cloned())
    }
    async fn find_replay(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<crate::start::StartReplay>, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            let key = (self.scope.clone(), idempotency_key.to_owned());
            match db.start_replays.get(&key) {
                Some((_, created)) if now - *created >= crate::start::REPLAY_RETENTION_MS => {
                    db.start_replays.remove(&key);
                    Ok(None)
                }
                other => Ok(other.map(|(replay, _)| replay.clone())),
            }
        })
    }
    async fn cancel_queued(
        &self,
        id: &ExecutionId,
        replay: Option<&crate::actor::Replay>,
    ) -> Result<bool, StoreError> {
        self.transaction(|db| {
            if db.owners.get(id) != Some(&self.scope) {
                return Err(StoreError::NotFound);
            }
            let now = self.clock.get();
            if let Some(replay) = replay
                && let Some(value) = replay.lookup(
                    "cancel_queued",
                    actor_replay::lookup(db, id, &replay.prefix("cancel_queued")?, now).iter(),
                    now,
                )?
            {
                return Ok(value);
            }
            let value = db.catalog.cancel_queued(&self.scope, id);
            if value {
                // CF6: a run cancelled while queued never counts.
                quota::refund(db, id);
            }
            if let Some(replay) = replay {
                let row = replay.record("cancel_queued", id, now, value)?;
                actor_replay::write(db, row, now)?;
                self.checkpoint(WriteStage::Idempotency)?;
            }
            Ok(value)
        })
    }
}

impl crate::execution_credentials::ExecutionCredentialStore for MemoryExecutionStore {
    async fn credential_family(
        &self,
        family_id: &str,
    ) -> Result<Option<crate::execution_credentials::CredentialFamily>, StoreError> {
        Ok(self
            .database
            .borrow()
            .credential_families
            .get(family_id)
            .filter(|(scope, _)| scope == &self.scope)
            .map(|(_, family)| family.clone()))
    }
    async fn credential_by_hash(
        &self,
        hash: &crate::credentials::SecretHash,
    ) -> Result<Option<crate::execution_credentials::ExecutionCredentialRecord>, StoreError> {
        Ok(self
            .database
            .borrow()
            .exec_credentials
            .values()
            .find(|record| {
                &record.hash == hash
                    && record.org_id == self.scope.org_id
                    && record.project_id == self.scope.project_id
            })
            .cloned())
    }
    async fn credential_by_id(
        &self,
        token_id: &runbook_model::TokenId,
    ) -> Result<Option<crate::execution_credentials::ExecutionCredentialRecord>, StoreError> {
        Ok(self
            .database
            .borrow()
            .exec_credentials
            .get(token_id)
            .filter(|record| {
                record.org_id == self.scope.org_id && record.project_id == self.scope.project_id
            })
            .cloned())
    }
    async fn family_by_idempotency_key(
        &self,
        execution_id: &ExecutionId,
        key: &str,
    ) -> Result<Option<crate::execution_credentials::CredentialFamily>, StoreError> {
        Ok(self
            .database
            .borrow()
            .credential_families
            .values()
            .find(|(scope, family)| {
                scope == &self.scope
                    && &family.execution_id == execution_id
                    && family.idempotency_key == key
            })
            .map(|(_, family)| family.clone()))
    }
}

mod step_lookup;

impl crate::webhooks::ProjectOutbox for MemoryExecutionStore {
    async fn pending_project_events(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::webhooks::ProjectEvent>, StoreError> {
        let db = self.database.borrow();
        let mut events: Vec<_> = db
            .project_outbox
            .iter()
            .filter(|((scope, _), _)| *scope == self.scope)
            .map(|(_, event)| event.clone())
            .collect();
        events.sort_by(|a, b| (a.at_ms, &a.event_id).cmp(&(b.at_ms, &b.event_id)));
        events.truncate(limit);
        Ok(events)
    }
    async fn ack_project_event(&self, event_id: &str) -> Result<(), StoreError> {
        self.transaction(|db| {
            db.project_outbox
                .remove(&(self.scope.clone(), event_id.to_owned()));
            Ok(())
        })
    }
}

/// CF6 quotas on the memory adapter.
mod quota {
    use super::{Database, MemoryExecutionStore};
    use crate::{
        StoreError,
        catalog::TenantScope,
        quotas::{MONTHLY_CODE, OrgUsage, QuotaLimits, QuotaStore, has_room, month_of},
    };
    use runbook_model::{ExecutionId, ExecutionStatus, OrganizationId};

    pub(super) fn reserve(
        db: &mut Database,
        limits: &QuotaLimits,
        scope: &TenantScope,
        id: &ExecutionId,
        now: i64,
    ) -> Result<(), StoreError> {
        if db.quota_reservations.contains_key(id) {
            return Ok(());
        }
        let key = (scope.org_id.clone(), month_of(now));
        let used = db.usage.get(&key).copied().unwrap_or(0);
        if !has_room(limits, used) {
            return Err(StoreError::InvalidBatch(MONTHLY_CODE));
        }
        db.usage.insert(key.clone(), used + 1);
        db.quota_reservations.insert(id.clone(), key);
        Ok(())
    }

    pub(super) fn refund(db: &mut Database, id: &ExecutionId) {
        if let Some(key) = db.quota_reservations.remove(id)
            && let Some(used) = db.usage.get_mut(&key)
        {
            *used = used.saturating_sub(1);
        }
    }

    impl QuotaStore for MemoryExecutionStore {
        async fn org_usage(&self, org: &OrganizationId) -> Result<OrgUsage, StoreError> {
            if org != &self.scope.org_id {
                return Err(StoreError::NotFound);
            }
            let month = month_of(self.clock.get());
            let db = self.database.borrow();
            let running = db
                .runs
                .iter()
                .filter(|(id, run)| {
                    db.owners.get(*id).is_some_and(|s| &s.org_id == org)
                        && !matches!(
                            run.projection.status,
                            ExecutionStatus::Queued
                                | ExecutionStatus::Succeeded
                                | ExecutionStatus::Failed
                                | ExecutionStatus::Cancelled
                        )
                })
                .count();
            Ok(OrgUsage {
                executions_started: db
                    .usage
                    .get(&(org.clone(), month.clone()))
                    .copied()
                    .unwrap_or(0),
                month,
                running: running as u64,
                limits: self.quota,
            })
        }
    }
}
