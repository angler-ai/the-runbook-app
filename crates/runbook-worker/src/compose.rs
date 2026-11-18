//! CF3: one tenant's execution-side stores on Cloudflare, composed from an
//! ExecutionDO's store (CF2) and D1 (`runbook-store-d1`), the shape
//! `runbook_api::backends::Backends::Execution` asks for.
//!
//! - Execution state (load, commit, sessions, claims, execution
//!   credentials, the work queue, the start record) is the run's object's,
//!   reached through [`Objects`]. Inside an ExecutionDO only its own run is
//!   reachable (CF2-5: execution-scoped requests run inside the object);
//!   native tests reach every object of a map.
//! - Catalog, admission, memory and the step index are D1's.
//! - Derived rows reach D1 through [`CfStore::drain_derived`], the
//!   applier's loop: pending rows in sequence order, applied idempotently,
//!   acknowledged through the last one applied. Admission notices
//!   (waiters promoted in D1) are delivered to their objects with
//!   [`CfStore::deliver_notices`].
//!
//! Portable: the same code runs natively over rusqlite and in workerd.
use crate::object::ObjectHandle;
use runbook_core::{
    ClaimWrite, CommitResult, ExecutionCommit, ExecutionStore, StoreError, StoredExecution,
    actor::Replay,
    catalog::{
        Admission, AdmitRequest, Assignment, CatalogStore, DraftRevision, RunSummary,
        RunbookRecord, SkillVersion, TenantScope, VersionRecord,
    },
    claims::ClaimStore,
    credentials::SecretHash,
    execution_credentials::{
        CredentialFamily, ExecutionCredentialRecord, ExecutionCredentialStore,
    },
    memory_entries::{
        MemoryEntryRecord, MemoryPatch, MemoryProposal, MemoryQuery, MemoryRecordPage,
        MemoryRevisionRecord, MemoryStore,
    },
    session::SessionStore,
    start::{StartCommit, StartOutcome, StartRecord, StartReplay, StartStore},
    step_lookup::StepRunLookup,
    work_queue::{WorkId, WorkLease, WorkQueue},
};
use runbook_model::{
    ActorRef, ClaimRef, DraftView, ExecutionId, MemoryId, OrganizationId, ProjectId,
    PublishedVersionView, RunbookId, StepRunId, TokenId, VersionId, VersionSelector, VersionView,
};
use runbook_store_d1::{
    D1, D1Catalog, D1Memory,
    admission::{Promotion, ack_notice},
};
use runbook_store_do::StartInit;
use runbook_store_r2::{ObjectBucket, R2BlobStore};

/// Where a run's ExecutionDO store is reachable from this host: its own
/// storage inside the object, the object protocol elsewhere (CF5).
pub trait Objects: Clone + 'static {
    type Handle: ObjectHandle;
    /// CF6: an organization's OrgDO.
    type Quota: crate::quota::QuotaHandle;
    fn quota(&self, org: &OrganizationId) -> Self::Quota;
    /// CF11: the webhook subscriptions' objects (their delivery logs).
    type Webhooks: crate::webhooks::WebhookObjects;
    fn webhooks(&self) -> Self::Webhooks;
    /// The object of `id`, bound to `scope` (§18: the scope comes from a
    /// verified credential). `NotFound` when it is not reachable here.
    fn object(&self, id: &ExecutionId, scope: &TenantScope) -> Result<Self::Handle, StoreError>;
    /// Every object reachable here that can be listed (a DO: itself; tests:
    /// all of them; the api Worker: none, it reaches objects by ID).
    fn reachable(&self) -> Vec<ExecutionId>;
    /// Called with the object a commit is about to go to (test faults).
    fn before_commit(&self, _object: &Self::Handle) {}
    /// Test seam: whether a fault was armed for `stage` outside a commit
    /// (a run deletion's), consuming it.
    fn take_fault(&self, _stage: runbook_core::WriteStage) -> bool {
        false
    }
}

/// How many derived rows one drain reads per page.
pub const DRAIN_PAGE: usize = 50;

/// One tenant's composed store.
pub struct CfStore<D: D1, O: Objects, B: ObjectBucket> {
    scope: TenantScope,
    /// CF11: the tenant's data is on `shards.for_org(scope.org_id)` (the
    /// catalog, memory, uploads and blob owners below); token routes and
    /// the directory on `shards.control()`.
    shards: crate::shards::D1Shards<D>,
    objects: O,
    catalog: D1Catalog<D>,
    memory: D1Memory<D>,
    uploads: runbook_store_d1::D1Uploads<D>,
    blobs: R2BlobStore<B, crate::owners::D1Owners<D>>,
    /// Test hosts: settle each commit (drain its derived rows and deliver
    /// admission notices) before it returns, as the ExecutionDO's host does
    /// after every request.
    settle_commits: bool,
    /// CF6 quota limits (configuration; off by default).
    quota: runbook_core::quotas::QuotaLimits,
    /// CF13 review round 1: the run budget (`RUNBOOK_LIMIT_RUN_BYTES`).
    run_budget: u64,
}
impl<D: D1, O: Objects, B: ObjectBucket> Clone for CfStore<D, O, B> {
    fn clone(&self) -> Self {
        Self {
            scope: self.scope.clone(),
            shards: self.shards.clone(),
            objects: self.objects.clone(),
            catalog: self.catalog.clone(),
            memory: self.memory.clone(),
            uploads: self.uploads.clone(),
            blobs: self.blobs.clone(),
            settle_commits: self.settle_commits,
            quota: self.quota,
            run_budget: self.run_budget,
        }
    }
}

/// CF11: a project's content-addressed R2 blobs (`BlobKey::Shared`, beside
/// its snapshots and skill files) as the catalog's runbook-source store.
struct R2Sources<B: ObjectBucket, O: runbook_store_r2::BlobOwners>(R2BlobStore<B, O>);
impl<B: ObjectBucket, O: runbook_store_r2::BlobOwners> runbook_store_d1::sources::SourceBlobs
    for R2Sources<B, O>
{
    fn put(
        &self,
        digest: [u8; 32],
        bytes: Vec<u8>,
    ) -> runbook_store_d1::sources::SourceFuture<'_, ()> {
        use runbook_core::blob::BlobStore;
        Box::pin(async move {
            self.0
                .put(runbook_core::blob::BlobKey::Shared { hash: digest }, &bytes)
                .await
        })
    }
    fn get(
        &self,
        digest: [u8; 32],
    ) -> runbook_store_d1::sources::SourceFuture<'_, Option<Vec<u8>>> {
        use runbook_core::blob::BlobStore;
        Box::pin(async move {
            match self
                .0
                .get(&runbook_core::blob::BlobKey::Shared { hash: digest })
                .await
            {
                Ok(bytes) => Ok(Some(bytes)),
                Err(StoreError::NotFound) => Ok(None),
                Err(error) => Err(error),
            }
        })
    }
}

/// What one drain did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Drained {
    /// The last sequence applied and acknowledged, if any.
    pub through: Option<u64>,
    pub applied: usize,
    /// Waiters the drained rows' releases promoted.
    pub promoted: Vec<Promotion>,
}

impl<D: D1, O: Objects, B: ObjectBucket> CfStore<D, O, B> {
    /// `blobs` and `user_content` are the two buckets (D57); blob
    /// ownership is D1's (`owners::D1Owners`).
    pub fn new(db: D, objects: O, scope: TenantScope, blobs: B, user_content: B) -> Self {
        Self::sharded(
            crate::shards::D1Shards::single(db),
            objects,
            scope,
            blobs,
            user_content,
        )
    }
    /// The store of `scope` on its shard (CF11).
    pub fn sharded(
        shards: crate::shards::D1Shards<D>,
        objects: O,
        scope: TenantScope,
        blobs: B,
        user_content: B,
    ) -> Self {
        let db = shards.for_org(&scope.org_id);
        let blobs = R2BlobStore::new(
            blobs,
            user_content,
            crate::owners::D1Owners::new(db.clone()),
            scope.clone(),
        );
        Self {
            shards,
            // CF11: runbook source in the project's R2 blobs, its digest in D1.
            catalog: D1Catalog::new(db.clone(), scope.clone())
                .with_sources(std::rc::Rc::new(R2Sources(blobs.clone()))),
            memory: D1Memory::new(db.clone()),
            uploads: runbook_store_d1::D1Uploads::new(db),
            settle_commits: false,
            quota: Default::default(),
            run_budget: runbook_core::run_budget::DEFAULT_MAX_RUN_BYTES,
            blobs,
            objects,
            scope,
        }
    }
    /// The same store for another tenant (its shard and its project's
    /// blobs, CF11).
    #[must_use]
    pub fn scoped(&self, scope: TenantScope) -> Self {
        let (blobs, user_content) = self.blobs.buckets();
        let mut other = Self::sharded(
            self.shards.clone(),
            self.objects.clone(),
            scope,
            blobs.clone(),
            user_content.clone(),
        );
        other.settle_commits = self.settle_commits;
        other.quota = self.quota;
        other
    }
    pub fn blobs(&self) -> &R2BlobStore<B, crate::owners::D1Owners<D>> {
        &self.blobs
    }
    pub fn scope(&self) -> &TenantScope {
        &self.scope
    }
    pub fn catalog(&self) -> &D1Catalog<D> {
        &self.catalog
    }
    pub fn memory(&self) -> &D1Memory<D> {
        &self.memory
    }
    /// The D1 database this store's tenant lives in (its shard).
    pub fn d1(&self) -> &D {
        self.catalog.db()
    }
    /// The control database (CF11): the directory, credentials and token
    /// routes.
    pub fn control(&self) -> D {
        self.shards.control()
    }
    /// Whether the tenant's shard is the control database (one batch can
    /// span both).
    pub fn colocated(&self) -> bool {
        self.shards.colocated(&self.scope.org_id)
    }
    pub fn shards(&self) -> &crate::shards::D1Shards<D> {
        &self.shards
    }
    pub fn objects(&self) -> &O {
        &self.objects
    }
    fn object(&self, id: &ExecutionId) -> Result<O::Handle, StoreError> {
        self.objects.object(id, &self.scope)
    }
    /// Test hosts: settle every commit before it returns (see the field).
    pub fn settle_commits(&mut self, on: bool) {
        self.settle_commits = on;
    }
    /// CF6: the same store with `limits` in force.
    #[must_use]
    pub fn with_quota(&self, limits: runbook_core::quotas::QuotaLimits) -> Self {
        Self {
            quota: limits,
            ..self.clone()
        }
    }
    /// CF13 review round 1: the same store with run budget `bytes`.
    #[must_use]
    pub fn with_run_budget(&self, bytes: u64) -> Self {
        Self {
            run_budget: bytes,
            ..self.clone()
        }
    }
    /// CF6: this tenant's organization's OrgDO.
    pub fn org_quota(&self) -> O::Quota {
        self.objects.quota(&self.scope.org_id)
    }
    /// The conformance `create`: a run's head at revision 0 in its object.
    /// A deleted ID is never made again.
    pub async fn create_execution(
        &self,
        projection: runbook_core::ExecutionProjection,
    ) -> Result<(), StoreError> {
        if self.catalog.tombstoned(&projection.execution_id).await? {
            return Err(StoreError::NotFound);
        }
        self.object(&projection.execution_id)?
            .create_execution(projection)
            .await
    }
    /// What the host does after a request that committed to `id`: its
    /// derived rows into D1, then admission notices. Failures stay for the
    /// object's alarm (D56).
    async fn settle_after(&self, id: &ExecutionId) {
        if let Ok(drained) = self.drain_derived(id).await {
            let _ = self.deliver_notices(&drained.promoted).await;
        }
    }
    /// CF4-8 (CF5): move `step_run_id`'s full log chunks to R2, each in
    /// two steps around its R2 write. A chunk's bytes are a function of the
    /// inline lines, so a retry after a crash puts the same bytes again.
    ///
    /// A deploy that changes how chunks are cut (CLI3: blank lines count
    /// one byte, not zero) can leave a chunk put by the earlier code under
    /// the key, never recorded. R2 keys are immutable, so the new chunk's
    /// put is refused; the chunk already there is recorded instead (CLI3
    /// re-check N9, [`Self::chunk_in_r2`]).
    async fn flush_logs(
        &self,
        object: &O::Handle,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<(), StoreError> {
        while let Some(pending) = object.log_flush(id, step_run_id).await? {
            let bytes = serde_json::to_vec(&pending.lines)
                .map_err(|_| StoreError::Backend("logs.encode"))?;
            let key = runbook_core::blob::BlobKey::Execution {
                execution_id: id.clone(),
                name: runbook_store_do::logs::chunk_name(step_run_id, pending.chunk.chunk),
            };
            let chunk =
                match runbook_core::blob::BlobStore::put(&self.blobs, key.clone(), &bytes).await {
                    Ok(()) => pending.chunk,
                    Err(StoreError::InvalidBatch("blob.immutable_key")) => {
                        self.chunk_in_r2(&key, &pending).await?
                    }
                    Err(e) => return Err(e),
                };
            object.log_flushed(id, step_run_id, &chunk).await?;
        }
        Ok(())
    }

    /// The chunk an earlier flush put under `pending`'s key and never
    /// recorded. Chunks are numbered in order and an unrecorded number's
    /// lines are never dropped, so that chunk also starts at the oldest
    /// inline line; it agrees with `pending` on the lines both hold and
    /// differs only in where it ends. `log_flushed` checks that its lines
    /// are still inline and its size (by today's measure) matches before
    /// recording it, so nothing is lost or read twice.
    async fn chunk_in_r2(
        &self,
        key: &runbook_core::blob::BlobKey,
        pending: &runbook_store_do::logs::PendingChunk,
    ) -> Result<runbook_store_do::logs::LogChunk, StoreError> {
        let stale = || StoreError::InvalidBatch("log.flush_stale");
        let existing: Vec<String> =
            serde_json::from_slice(&runbook_core::blob::BlobStore::get(&self.blobs, key).await?)
                .map_err(|_| StoreError::Backend("logs.chunk_invalid"))?;
        if existing.is_empty()
            || existing
                .iter()
                .zip(&pending.lines)
                .any(|(there, inline)| there != inline)
        {
            return Err(stale());
        }
        Ok(runbook_store_do::logs::LogChunk {
            chunk: pending.chunk.chunk,
            first_seq: pending.chunk.first_seq,
            last_seq: pending.chunk.first_seq + existing.len() as u64 - 1,
            bytes: existing
                .iter()
                .map(|line| runbook_core::claims::log_entry_bytes(line))
                .sum(),
        })
    }

    /// LANG3 `cancel_previous` (CF5): have an evicted run's object cancel
    /// it, recorded as replaced by `superseded`, prepared against its
    /// current state; then its derived rows (a closing run's release
    /// promotes the start, first in line) and the notice is forgotten.
    pub async fn deliver_supersede(
        &self,
        run: &runbook_core::concurrency::PoolHolder,
        superseded: &runbook_engine::Supersession,
    ) -> Result<(), StoreError> {
        if run.state == runbook_core::catalog::AdmissionState::Queued {
            // CF6: an evicted waiter was cancelled while queued.
            use crate::quota::QuotaHandle;
            self.org_quota().refund(&run.execution_id).await?;
        }
        let directory = runbook_store_d1::D1Directory::new(self.control());
        match runbook_core::concurrency::cancel_superseded(
            self,
            &directory,
            &self.scope,
            run,
            superseded,
        )
        .await
        {
            Ok(_) | Err(StoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
        if let Ok(drained) = self.drain_derived(&run.execution_id).await {
            self.deliver_notices(&drained.promoted).await?;
        }
        runbook_store_d1::start::ack_supersede(self.catalog.db(), &run.execution_id).await
    }

    /// Empty a deleted run's object and R2 prefix, then forget the purge.
    /// A failure leaves the `object_purge` row for the minute cron.
    pub async fn finish_purge(
        &self,
        id: &ExecutionId,
        keep_uploads: bool,
    ) -> Result<(), StoreError> {
        match self.object(id) {
            Ok(object) => match object.purge(id).await {
                Ok(_) | Err(StoreError::NotFound) => {}
                Err(error) => return Err(error),
            },
            // Not reachable from here: the cron finishes it.
            Err(StoreError::NotFound) => return Ok(()),
            Err(error) => return Err(error),
        }
        self.blobs.purge_run(id, keep_uploads).await?;
        // CF11: its deliveries in the subscriptions' WebhookDOs.
        crate::webhooks::CfWebhooks::sharded(self.shards.clone(), self.objects.webhooks())
            .forget_execution(&self.scope, id)
            .await?;
        // CF11: the run's token routes are the control database's, outside
        // the deletion's shard batch; forgotten before the purge is.
        runbook_store_d1::credentials::forget_execution_routes(&self.control(), id).await?;
        runbook_store_d1::retention::purge_done(self.catalog.db(), id).await
    }

    /// Apply `id`'s pending derived rows to D1 in sequence order and
    /// acknowledge them (CF2-6, D56). Each row is idempotent by (execution,
    /// sequence), so a crash between apply and ack redelivers harmlessly.
    /// The bounded delay: the host drains after every commit, before its
    /// request returns; a failed apply defers the stream, and the object's
    /// alarm drains again `OUTBOX_FIRST_RETRY_MS` after the oldest pending
    /// row, backing off to `OUTBOX_MAX_RETRY_MS` while D1 stays down.
    pub async fn drain_derived(&self, id: &ExecutionId) -> Result<Drained, StoreError> {
        let object = self.object(id)?;
        let mut drained = Drained::default();
        let mut step_runs: Option<Vec<StepRunId>> = None;
        loop {
            let page = object.pending_derived(drained.through, DRAIN_PAGE).await?;
            if page.is_empty() {
                break;
            }
            if step_runs.is_none() {
                let run = object.load(id).await?;
                step_runs = Some(
                    run.projection
                        .steps
                        .iter()
                        .map(|s| s.step_run_id.clone())
                        .collect(),
                );
            }
            for (sequence, update) in &page {
                let applied = match self
                    .catalog
                    .apply_derived(update, step_runs.as_deref().unwrap_or_default())
                    .await
                {
                    Ok(applied) => applied,
                    Err(error) => {
                        // CF2-6: back the stream off; the object's alarm
                        // retries from OUTBOX_FIRST_RETRY_MS after the
                        // oldest pending row, doubling to the maximum.
                        object
                            .defer_outbox(runbook_store_do::OutboxStream::Derived)
                            .await?;
                        return Err(error);
                    }
                };
                drained.promoted.extend(applied.promoted);
                drained.applied += 1;
                object.ack_derived(*sequence).await?;
                drained.through = Some(*sequence);
            }
        }
        Ok(drained)
    }

    /// Re-apply one recorded derived row (a redelivery; idempotent).
    pub async fn redeliver_derived(
        &self,
        id: &ExecutionId,
        sequence: u64,
    ) -> Result<(), StoreError> {
        let object = self.object(id)?;
        let update = object
            .derived_row(sequence)
            .await?
            .ok_or(StoreError::NotFound)?;
        let steps: Vec<StepRunId> = object
            .load(id)
            .await?
            .projection
            .steps
            .iter()
            .map(|s| s.step_run_id.clone())
            .collect();
        self.catalog.apply_derived(&update, &steps).await?;
        Ok(())
    }

    /// Tell promoted waiters' objects they are admitted (D24), then forget
    /// each notice. A notice whose object is not reachable here stays for
    /// a host that can reach it (the minute cron).
    pub async fn deliver_notices(&self, notices: &[Promotion]) -> Result<usize, StoreError> {
        let mut delivered = 0;
        for notice in notices {
            let Ok(object) = self.object(&notice.execution_id) else {
                continue;
            };
            match object
                .admit(&notice.execution_id, notice.admitted_at_ms)
                .await
            {
                // A refusal is terminal (the run has started, or holds
                // another admission time), as the `/admit` route's 409.
                Ok(()) | Err(StoreError::InvalidBatch(_)) => {}
                // Gate fix D1: an empty object may belong to a half-committed
                // start whose retry initializes it: keep the notice for it.
                Err(StoreError::NotFound) => {
                    if !runbook_store_d1::admission::notice_orphaned(
                        self.catalog.db(),
                        &notice.execution_id,
                    )
                    .await?
                    {
                        continue;
                    }
                }
                Err(error) => return Err(error),
            }
            ack_notice(self.catalog.db(), notice).await?;
            delivered += 1;
        }
        Ok(delivered)
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> ExecutionStore for CfStore<D, O, B> {
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        self.object(id)?.load(id).await
    }
    /// The D1 step first ([`before_commit`]: memory quota, token routes).
    /// A commit that would create a run (revision 0) is refused for an ID
    /// a deletion tombstoned: deleted runs never come back.
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        let id = batch.execution_id.clone();
        let object = self.object(&id)?;
        if batch.expected_sequence == 0 && self.catalog.tombstoned(&id).await? {
            return Err(StoreError::NotFound);
        }
        before_commit(&self.scope, &self.control(), &self.memory, &batch).await?;
        self.objects.before_commit(&object);
        let result = object.commit(batch).await?;
        if self.settle_commits && matches!(result, CommitResult::Committed(_)) {
            self.settle_after(&id).await;
        }
        Ok(result)
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> SessionStore for CfStore<D, O, B> {
    type Session = CfSession<D, O, B>;
    fn run_budget(&self) -> u64 {
        self.run_budget
    }
    async fn begin(&self, id: &ExecutionId) -> Result<Self::Session, StoreError> {
        Ok(CfSession {
            inner: self.object(id)?.begin(id).await?,
            store: self.clone(),
        })
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> ClaimStore for CfStore<D, O, B> {
    async fn heartbeat(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        replay: Option<&Replay>,
    ) -> Result<Option<i64>, StoreError> {
        self.object(id)?
            .heartbeat(id, step_run_id, claim, owner, replay)
            .await
    }
    async fn append_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        lines: Vec<String>,
        replay: Option<&Replay>,
    ) -> Result<u64, StoreError> {
        let object = self.object(id)?;
        let seq = object
            .append_logs(id, step_run_id, claim, owner, lines, replay)
            .await?;
        // CF5: full chunks move to R2 after the object's transaction; a
        // failure leaves the lines inline for the next append.
        let _ = self.flush_logs(&object, id, step_run_id).await;
        Ok(seq)
    }
    async fn claim_record(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<Option<ClaimWrite>, StoreError> {
        self.object(id)?.claim_record(id, step_run_id).await
    }
    async fn read_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        // CF5: the chunks in R2 first, then the inline tail.
        let object = self.object(id)?;
        let mut out: Vec<(u64, String)> = vec![];
        let mut cursor = after;
        for chunk in object.log_chunks(id, step_run_id, after).await? {
            if out.len() >= limit {
                return Ok(out);
            }
            let bytes = runbook_core::blob::BlobStore::get(
                &self.blobs,
                &runbook_core::blob::BlobKey::Execution {
                    execution_id: id.clone(),
                    name: runbook_store_do::logs::chunk_name(step_run_id, chunk.chunk),
                },
            )
            .await?;
            let lines: Vec<String> = serde_json::from_slice(&bytes)
                .map_err(|_| StoreError::Backend("logs.chunk_invalid"))?;
            for (seq, line) in (chunk.first_seq..).zip(lines) {
                if seq > cursor && out.len() < limit {
                    out.push((seq, line));
                    cursor = seq;
                }
            }
        }
        if out.len() < limit {
            out.extend(
                object
                    .read_logs(id, step_run_id, cursor, limit - out.len())
                    .await?,
            );
        }
        Ok(out)
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> ExecutionCredentialStore for CfStore<D, O, B> {
    async fn credential_family(
        &self,
        family_id: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        for id in self.objects.reachable() {
            if let Ok(object) = self.object(&id)
                && let Some(found) = object.credential_family(family_id).await?
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
    async fn credential_by_hash(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        for id in self.objects.reachable() {
            if let Ok(object) = self.object(&id)
                && let Some(found) = object.credential_by_hash(hash).await?
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
    async fn credential_by_id(
        &self,
        token_id: &TokenId,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        for id in self.objects.reachable() {
            if let Ok(object) = self.object(&id)
                && let Some(found) = object.credential_by_id(token_id).await?
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
    async fn family_by_idempotency_key(
        &self,
        execution_id: &ExecutionId,
        key: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        self.object(execution_id)?
            .family_by_idempotency_key(execution_id, key)
            .await
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> WorkQueue for CfStore<D, O, B> {
    async fn claim_due(&self, limit: usize, lease_ms: u64) -> Result<Vec<WorkLease>, StoreError> {
        let mut out = vec![];
        for id in self.objects.reachable() {
            if out.len() >= limit {
                break;
            }
            if let Ok(object) = self.object(&id) {
                match object.claim_due(limit - out.len(), lease_ms).await {
                    Ok(leases) => out.extend(leases),
                    Err(StoreError::NotFound) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(out)
    }
    async fn ack(&self, id: &WorkId, generation: u64) -> Result<(), StoreError> {
        self.object(&id.execution_id)?.ack(id, generation).await
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> StartStore for CfStore<D, O, B> {
    /// D1 first (the commit point: replay, owner, admission, saved
    /// outcome), then the run's object. A replayed start re-initializes an
    /// object whose write was lost; an initialized one is left as it is.
    async fn commit_start(&self, commit: StartCommit) -> Result<StartOutcome, StoreError> {
        use crate::quota::QuotaHandle;
        let id = &commit.projection.execution_id;
        // CF6: count the start in the OrgDO first (idempotent per run),
        // unless its key already answers; the D1 batch is the commit point,
        // so a start it refuses or answers elsewhere is refunded.
        let replay_saved = match &commit.replay {
            Some(replay) => self.catalog.find_start_replay(&replay.key).await?.is_some(),
            None => false,
        };
        let quota = self.org_quota();
        if !replay_saved {
            let now = self.catalog.db().now_ms();
            quota
                .reserve(id, &runbook_core::quotas::month_of(now), now, self.quota)
                .await?;
        }
        let reserved = match self.catalog.reserve_start(&commit).await {
            Ok(reserved) => reserved,
            Err(error) => {
                if !replay_saved {
                    let _ = quota.refund(id).await;
                }
                return Err(error);
            }
        };
        if reserved.replayed && &reserved.outcome.execution_id != id {
            let _ = quota.refund(id).await;
        }
        if reserved.replayed {
            // Review fix 1: a request that missed the replay lookup reached
            // a fresh object; the key's run is another one. Never start
            // this one: the host sends the request to the saved run.
            if &reserved.outcome.execution_id != id {
                return Err(StoreError::InvalidBatch(REPLAYED_ELSEWHERE));
            }
            if reserved.outcome.runbook_version_id != commit.record.version_id {
                return Err(StoreError::InvalidBatch("start.replay_version_changed"));
            }
        }
        let object = self.object(id)?;
        let initialized = match object.load(id).await {
            Ok(_) => true,
            Err(StoreError::NotFound) => false,
            Err(error) => return Err(error),
        };
        if !initialized {
            object
                .initialize_start(StartInit {
                    projection: commit.projection.clone(),
                    record: commit.record.clone(),
                    admission: reserved.outcome.admission.clone(),
                    derived: commit.derived.clone(),
                })
                .await?;
        } else if !reserved.replayed {
            return Err(StoreError::InvalidBatch("store.execution_exists"));
        }
        let mut follow_d1 = None;
        if reserved.outcome.admission.as_ref().map(|a| a.state)
            == Some(runbook_core::catalog::AdmissionState::Queued)
        {
            // Gate fix D1: the saved outcome may be older than D1, which can
            // have promoted this start while its object was empty (a
            // half-committed start whose notice met no run): follow D1.
            if let Some(current) = self.catalog.admission(id).await?
                && current.state == runbook_core::catalog::AdmissionState::Admitted
            {
                if let Some(at) = current.admitted_at_ms {
                    match object.admit(id, at).await {
                        Ok(()) | Err(StoreError::InvalidBatch(_)) => {}
                        Err(error) => return Err(error),
                    }
                }
                follow_d1 = Some(current);
            }
        }
        self.deliver_notices(&reserved.promoted).await?;
        // LANG3 (CF5): each evicted run's object cancels it now; a failure
        // leaves its notice to the minute cron.
        let superseded = runbook_engine::Supersession {
            execution_id: id.clone(),
            started_by: commit.record.context.created_by.clone(),
        };
        for run in &reserved.superseded {
            let _ = self.deliver_supersede(run, &superseded).await;
        }
        // The start's own row (sequence 0) reaches D1 before the start
        // answers, so the response and an immediate list read see the run
        // (Postgres writes it in the start transaction). A failure leaves
        // it to the object's settle and alarm.
        let _ = self.drain_derived(id).await;
        let mut outcome = reserved.outcome;
        if let Some(admission) = follow_d1 {
            outcome.admission = Some(admission);
            if let Some(replay) = &commit.replay {
                self.catalog
                    .update_start_replay(&replay.key, &outcome)
                    .await?;
            }
        }
        if !reserved.superseded.is_empty()
            && let Some(admission) = self.catalog.admission(id).await?
            && outcome.admission.as_ref() != Some(&admission)
        {
            // A holder with nothing left to clean up closed at once and
            // admitted this start: answer (and replay) admitted.
            outcome.admission = Some(admission);
            if let Some(replay) = &commit.replay {
                self.catalog
                    .update_start_replay(&replay.key, &outcome)
                    .await?;
            }
        }
        Ok(outcome)
    }
    async fn start_record(&self, id: &ExecutionId) -> Result<Option<StartRecord>, StoreError> {
        match self.object(id) {
            Ok(object) => match object.start_record(id).await {
                Err(StoreError::NotFound) => Ok(None),
                other => other,
            },
            Err(StoreError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }
    async fn find_replay(&self, key: &str) -> Result<Option<StartReplay>, StoreError> {
        self.catalog.find_start_replay(key).await
    }
    /// B6: release a waiter (never promoted later). The actor replay of
    /// the request lives in the object; a cancellation retried after its
    /// replay row is answered by the admission state alone.
    async fn cancel_queued(
        &self,
        id: &ExecutionId,
        _replay: Option<&Replay>,
    ) -> Result<bool, StoreError> {
        self.object(id)?.load(id).await?;
        let released = self.catalog.release_queued(id).await?;
        if released {
            // CF6: a run cancelled while queued never counts.
            use crate::quota::QuotaHandle;
            let _ = self.org_quota().refund(id).await;
        }
        Ok(released)
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> CatalogStore for CfStore<D, O, B> {
    async fn read_draft(&self, key: &str) -> Result<Option<DraftRevision>, StoreError> {
        self.catalog.read_draft(key).await
    }
    async fn replace_draft(
        &self,
        key: &str,
        expected: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        self.catalog.replace_draft(key, expected, source).await
    }
    async fn list_runs(&self) -> Result<Vec<RunSummary>, StoreError> {
        self.catalog.list_runs().await
    }
    async fn get_run(&self, id: &ExecutionId) -> Result<Option<RunSummary>, StoreError> {
        self.catalog.get_run(id).await
    }
    async fn list_assignments(&self) -> Result<Vec<Assignment>, StoreError> {
        self.catalog.list_assignments().await
    }
    async fn admit(&self, request: AdmitRequest) -> Result<Admission, StoreError> {
        self.catalog.admit(request).await
    }
    async fn admission(&self, id: &ExecutionId) -> Result<Option<Admission>, StoreError> {
        self.catalog.admission(id).await
    }
    async fn admission_pool(
        &self,
        key: &str,
    ) -> Result<runbook_core::concurrency::PoolView, StoreError> {
        self.catalog.admission_pool(key).await
    }
    async fn create_runbook(
        &self,
        record: RunbookRecord,
        source: String,
    ) -> Result<DraftView, StoreError> {
        self.catalog.create_runbook(record, source).await
    }
    async fn read_runbook(&self, id: &RunbookId) -> Result<Option<RunbookRecord>, StoreError> {
        self.catalog.read_runbook(id).await
    }
    async fn get_draft(&self, id: &RunbookId) -> Result<Option<DraftView>, StoreError> {
        self.catalog.get_draft(id).await
    }
    async fn save_draft(
        &self,
        id: &RunbookId,
        expected_revision: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        self.catalog.save_draft(id, expected_revision, source).await
    }
    async fn publish_revision(
        &self,
        id: &RunbookId,
        expected_revision: u64,
        prepared: runbook_core::publish::PreparedPublication,
    ) -> Result<PublishedVersionView, StoreError> {
        self.catalog
            .publish_revision(id, expected_revision, prepared)
            .await
    }
    async fn create_preview(
        &self,
        id: &RunbookId,
        prepared: runbook_core::publish::PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<VersionView, StoreError> {
        self.catalog
            .create_preview(id, prepared, branch, commit_sha)
            .await
    }
    async fn publish_git(
        &self,
        id: &RunbookId,
        prepared: runbook_core::publish::PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<runbook_model::PublishedVersionView, StoreError> {
        self.catalog
            .publish_git(id, prepared, branch, commit_sha)
            .await
    }
    async fn archive_git(&self, id: &RunbookId) -> Result<(), StoreError> {
        self.catalog.archive_git(id).await
    }
    async fn resolve_version(
        &self,
        id: &RunbookId,
        selector: Option<VersionSelector>,
    ) -> Result<VersionView, StoreError> {
        self.catalog.resolve_version(id, selector).await
    }
    async fn version_record(
        &self,
        runbook_id: &RunbookId,
        version_id: &VersionId,
    ) -> Result<Option<VersionRecord>, StoreError> {
        self.catalog.version_record(runbook_id, version_id).await
    }
    async fn list_versions(&self, id: &RunbookId) -> Result<Vec<PublishedVersionView>, StoreError> {
        self.catalog.list_versions(id).await
    }
    async fn list_previews(&self, id: &RunbookId) -> Result<Vec<VersionView>, StoreError> {
        self.catalog.list_previews(id).await
    }
    async fn record_skill_version(
        &self,
        version: SkillVersion,
    ) -> Result<SkillVersion, StoreError> {
        self.catalog.record_skill_version(version).await
    }
    async fn skill_version(&self, hash: &[u8; 32]) -> Result<Option<SkillVersion>, StoreError> {
        self.catalog.skill_version(hash).await
    }
    async fn latest_skills(&self) -> Result<Vec<SkillVersion>, StoreError> {
        self.catalog.latest_skills().await
    }
    async fn versions_pinning_skill(
        &self,
        hash: &[u8; 32],
    ) -> Result<Vec<(RunbookId, VersionRecord)>, StoreError> {
        self.catalog.versions_pinning_skill(hash).await
    }
    async fn execution_project(
        &self,
        org_id: &OrganizationId,
        id: &ExecutionId,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.catalog.execution_project(org_id, id).await
    }
    async fn tombstone_project(
        &self,
        org_id: &OrganizationId,
        id: &ExecutionId,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.catalog.tombstone_project(org_id, id).await
    }
    async fn search_runs(
        &self,
        filters: runbook_core::search::RunFilters,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<runbook_core::search::RunPage, StoreError> {
        self.catalog.search_runs(filters, cursor, limit).await
    }
    async fn runbook_run_stats(
        &self,
        runbook_ids: &[runbook_model::RunbookId],
        active_cap: usize,
    ) -> Result<
        std::collections::BTreeMap<runbook_model::RunbookId, runbook_core::search::RunbookRunStats>,
        StoreError,
    > {
        self.catalog
            .runbook_run_stats(runbook_ids, active_cap)
            .await
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> MemoryStore for CfStore<D, O, B> {
    async fn memory_time_ms(&self) -> Result<i64, StoreError> {
        self.memory.memory_time_ms().await
    }
    async fn create_memory(
        &self,
        scope: &TenantScope,
        entry: MemoryEntryRecord,
        actor: &str,
        limit: usize,
    ) -> Result<MemoryEntryRecord, StoreError> {
        self.memory.create_memory(scope, entry, actor, limit).await
    }
    async fn apply_memory_proposal(
        &self,
        scope: &TenantScope,
        proposal: MemoryProposal,
        limit: usize,
    ) -> Result<bool, StoreError> {
        self.memory
            .apply_memory_proposal(scope, proposal, limit)
            .await
    }
    async fn get_memory(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> Result<Option<MemoryEntryRecord>, StoreError> {
        self.memory.get_memory(scope, id).await
    }
    async fn memory_revisions(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> Result<Vec<MemoryRevisionRecord>, StoreError> {
        self.memory.memory_revisions(scope, id).await
    }
    async fn memory_revision(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        revision: u32,
    ) -> Result<Option<MemoryRevisionRecord>, StoreError> {
        self.memory.memory_revision(scope, id, revision).await
    }
    async fn memory_revision_page(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        after: u32,
        limit: usize,
    ) -> Result<Vec<MemoryRevisionRecord>, StoreError> {
        self.memory
            .memory_revision_page(scope, id, after, limit)
            .await
    }
    async fn patch_memory(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        expected_revision: u32,
        patch: MemoryPatch,
        actor: &str,
    ) -> Result<MemoryEntryRecord, StoreError> {
        self.memory
            .patch_memory(scope, id, expected_revision, patch, actor)
            .await
    }
    async fn search_memory(
        &self,
        scope: &TenantScope,
        query: &MemoryQuery,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<MemoryRecordPage, StoreError> {
        self.memory.search_memory(scope, query, cursor, limit).await
    }
    async fn memory_for_step(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        step_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntryRecord>, StoreError> {
        self.memory
            .memory_for_step(scope, runbook_id, step_id, limit)
            .await
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> StepRunLookup for CfStore<D, O, B> {
    async fn step_run_execution(
        &self,
        org_id: &OrganizationId,
        step_run_id: &StepRunId,
    ) -> Result<Option<(ExecutionId, ProjectId)>, StoreError> {
        self.catalog.step_run_execution(org_id, step_run_id).await
    }
}

impl<D: D1, O: Objects, B: ObjectBucket> runbook_core::blob::BlobStore for CfStore<D, O, B> {
    /// An archive version's put also records it in D1 (CF5,
    /// `archive_index.rs`).
    async fn put(&self, key: runbook_core::blob::BlobKey, bytes: &[u8]) -> Result<(), StoreError> {
        if matches!(key, runbook_core::blob::BlobKey::Archive { .. }) {
            return self.put_archive_version(key, bytes).await;
        }
        self.blobs.put(key, bytes).await
    }
    async fn get(&self, key: &runbook_core::blob::BlobKey) -> Result<Vec<u8>, StoreError> {
        self.blobs.get(key).await
    }
    async fn get_range(
        &self,
        key: &runbook_core::blob::BlobKey,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, StoreError> {
        self.blobs.get_range(key, range).await
    }
    async fn head(
        &self,
        key: &runbook_core::blob::BlobKey,
    ) -> Result<runbook_core::blob::BlobHead, StoreError> {
        self.blobs.head(key).await
    }
    async fn delete(&self, key: &runbook_core::blob::BlobKey) -> Result<(), StoreError> {
        if matches!(key, runbook_core::blob::BlobKey::Archive { .. }) {
            return self.delete_archive_version(key).await;
        }
        self.blobs.delete(key).await
    }
    async fn list_prefix(
        &self,
        prefix: &runbook_core::blob::BlobPrefix,
    ) -> Result<Vec<runbook_core::blob::BlobKey>, StoreError> {
        self.blobs.list_prefix(prefix).await
    }
}

/// Uploads and evidence metadata (UP1) on D1 (CF5): `runbook-store-d1`'s
/// `D1Uploads`. The bytes are R2's.
impl<D: D1, O: Objects, B: ObjectBucket> runbook_core::uploads::UploadStore for CfStore<D, O, B> {
    async fn upload_time_ms(&self) -> Result<i64, StoreError> {
        self.uploads.upload_time_ms().await
    }
    async fn reserve_evidence(
        &self,
        scope: &TenantScope,
        record: runbook_core::uploads::EvidenceRecord,
        quota: &runbook_core::uploads::UploadQuota,
    ) -> Result<bool, StoreError> {
        self.uploads.reserve_evidence(scope, record, quota).await
    }
    async fn get_evidence(
        &self,
        scope: &TenantScope,
        id: &runbook_model::EvidenceId,
    ) -> Result<Option<runbook_core::uploads::EvidenceRecord>, StoreError> {
        self.uploads.get_evidence(scope, id).await
    }
    async fn finalize_evidence(
        &self,
        scope: &TenantScope,
        id: &runbook_model::EvidenceId,
        outcome: runbook_core::uploads::UploadOutcome,
        quota: &runbook_core::uploads::UploadQuota,
    ) -> Result<runbook_core::uploads::EvidenceRecord, StoreError> {
        self.uploads
            .finalize_evidence(scope, id, outcome, quota)
            .await
    }
    async fn list_evidence(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> Result<Vec<runbook_core::uploads::EvidenceRecord>, StoreError> {
        self.uploads.list_evidence(scope, execution_id).await
    }
    async fn stale_evidence(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> Result<Vec<runbook_core::uploads::EvidenceRecord>, StoreError> {
        self.uploads.stale_evidence(scope, before_ms, limit).await
    }
    async fn delete_evidence(
        &self,
        scope: &TenantScope,
        id: &runbook_model::EvidenceId,
    ) -> Result<bool, StoreError> {
        self.uploads.delete_evidence(scope, id).await
    }
}

/// CF6: usage from the OrgDO (starts) and D1 (the running runs).
impl<D: D1, O: Objects, B: ObjectBucket> runbook_core::quotas::QuotaStore for CfStore<D, O, B> {
    async fn org_usage(
        &self,
        org: &OrganizationId,
    ) -> Result<runbook_core::quotas::OrgUsage, StoreError> {
        use crate::quota::QuotaHandle;
        if org != &self.scope.org_id {
            return Err(StoreError::NotFound);
        }
        let month = runbook_core::quotas::month_of(self.catalog.db().now_ms());
        Ok(runbook_core::quotas::OrgUsage {
            executions_started: self.org_quota().started(&month).await?,
            running: self.catalog.running_in_org(org).await?,
            month,
            limits: self.quota,
        })
    }
}

/// B3.4: the project event outbox is D1's.
impl<D: D1, O: Objects, B: ObjectBucket> runbook_core::webhooks::ProjectOutbox
    for CfStore<D, O, B>
{
    async fn pending_project_events(
        &self,
        limit: usize,
    ) -> Result<Vec<runbook_core::webhooks::ProjectEvent>, StoreError> {
        self.catalog.pending_project_events(limit).await
    }
    async fn ack_project_event(&self, event_id: &str) -> Result<(), StoreError> {
        self.catalog.ack_project_event(event_id).await
    }
}

/// B2.4/ARC2 on this edition (CF5): a run is its object, its D1 rows and
/// its R2 prefix. Export reads all three; deletion commits in D1 (the
/// tombstone, the owner row and its cascade, an `object_purge` row), then
/// empties the object and the prefix (`finish_purge`).
impl<D: D1, O: Objects, B: ObjectBucket> runbook_core::retention::RunStore for CfStore<D, O, B> {
    async fn export_execution(
        &self,
        id: &ExecutionId,
    ) -> Result<runbook_core::retention::ExecutionSlice, StoreError> {
        let object = self.object(id)?;
        let run = object.load(id).await?;
        if !runbook_store_d1::retention::finished(&run.projection.status) {
            return Err(StoreError::InvalidBatch("store.execution_open"));
        }
        let (actor_replays, work) = object.export_parts(id).await?;
        let catalog = self.catalog.export_catalog(id).await?;
        let blob_keys = runbook_core::blob::BlobStore::list_prefix(
            &self.blobs,
            &runbook_core::blob::BlobPrefix::Execution(id.clone()),
        )
        .await?
        .into_iter()
        .filter(|key| matches!(key, runbook_core::blob::BlobKey::Execution { .. }))
        .collect();
        Ok(runbook_core::retention::ExecutionSlice {
            scope: self.scope.clone(),
            run,
            actor_replays,
            catalog,
            work,
            blob_keys,
        })
    }
    async fn delete_execution(
        &self,
        id: &ExecutionId,
        reason: &str,
    ) -> Result<runbook_core::retention::Tombstone, StoreError> {
        use runbook_core::archives::RETENTION_REASON;
        if reason.trim().is_empty() {
            return Err(StoreError::InvalidBatch("store.deletion_reason_required"));
        }
        if let Some(existing) = self.catalog.tombstone(id).await? {
            // A single-run deletion of a run retention already deleted
            // takes it out of its file too (never a silent no-op).
            if existing.reason == RETENTION_REASON && reason != RETENTION_REASON {
                return runbook_core::archives::ArchiveIndex::purge_retained_run(
                    self,
                    &self.scope.clone(),
                    id,
                    reason,
                )
                .await?
                .ok_or(StoreError::NotFound);
            }
            return Ok(existing);
        }
        let object = self.object(id)?;
        let run = object.load(id).await?;
        if !runbook_store_d1::retention::finished(&run.projection.status) {
            return Err(StoreError::InvalidBatch("store.execution_open"));
        }
        let retention = reason == RETENTION_REASON;
        if retention && !self.archive_current(id, run.revision).await? {
            return Err(StoreError::InvalidBatch("archive.not_current"));
        }
        if self
            .objects
            .take_fault(runbook_core::WriteStage::RunDeletion)
        {
            return Err(StoreError::Backend("injected"));
        }
        let tombstone = self
            .catalog
            .delete_run(
                id,
                &run.projection.runbook_id,
                reason,
                &runbook_store_d1::retention::DeletionChecks {
                    archived_revision: retention.then_some(run.revision),
                },
            )
            .await?;
        // The deletion has committed; a failed purge is the cron's to finish.
        let _ = self.finish_purge(id, retention).await;
        Ok(tombstone)
    }
    async fn tombstone(
        &self,
        id: &ExecutionId,
    ) -> Result<Option<runbook_core::retention::Tombstone>, StoreError> {
        self.catalog.tombstone(id).await
    }
    async fn finished_on(
        &self,
        runbook: &RunbookId,
        utc_day: i64,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.catalog.finished_on(runbook, utc_day).await
    }
}

/// A start whose replay key already names another run (review fix 1): the
/// api Worker re-sends the request to that run's object.
pub const REPLAYED_ELSEWHERE: &str = "start.replayed_elsewhere";

/// What must happen in D1 before an object commits, on both commit paths
/// (plain commits and session commits):
/// - D40: a run's memory write over its runbook's quota (the proposal quota
///   for a proposal, the active quota for a MEM6 live save) fails the whole
///   commit, as on Postgres. The quota is D1's, so it is checked first; what
///   a race between two runs lets through, the applier stores as an archived
///   drop marker (`dropped: memory.limit_reached`).
/// - CF2-9: each new execution or refresh token's hash is routed to its run,
///   so the api Worker can find the run from a bearer. The row only routes;
///   the object rechecks the token.
async fn before_commit<D: D1>(
    tenant: &TenantScope,
    control: &D,
    memory: &D1Memory<D>,
    batch: &ExecutionCommit,
) -> Result<(), StoreError> {
    type Wanted = (runbook_model::MemoryScope, runbook_model::MemoryStatus, u64);
    let mut wanted: std::collections::BTreeMap<String, Wanted> = std::collections::BTreeMap::new();
    for proposal in batch.derived_outbox.iter().flat_map(|u| &u.memory) {
        let (scope, status) = (&proposal.entry.memory_scope, proposal.entry.status);
        wanted
            .entry(format!("{scope:?}/{status:?}"))
            .or_insert_with(|| (scope.clone(), status, 0))
            .2 += 1;
    }
    for (scope, status, needed) in wanted.values() {
        let used = memory.entries_counted(tenant, scope, *status).await?;
        if used + needed > runbook_core::memory_entries::run_quota(*status) as u64 {
            return Err(StoreError::InvalidBatch("memory.limit_reached"));
        }
    }
    // CF2-9: route each new execution or refresh token's hash to this
    // run before the object stores it, so the api Worker can find the
    // run from a bearer. The row only routes; the object rechecks.
    let credentials = runbook_store_d1::D1Credentials::new(control.clone());
    for write in &batch.credentials {
        let issued = match write {
            runbook_core::execution_credentials::CredentialWrite::IssueFamily {
                access,
                refresh,
                ..
            }
            | runbook_core::execution_credentials::CredentialWrite::Rotate {
                access,
                refresh,
                ..
            } => [access, refresh],
            runbook_core::execution_credentials::CredentialWrite::RevokeFamily { .. } => {
                continue;
            }
        };
        for record in issued {
            let scope = TenantScope {
                org_id: record.org_id.clone(),
                project_id: record.project_id.clone(),
            };
            if &scope != tenant || record.execution_id != batch.execution_id {
                return Err(StoreError::InvalidBatch("store.credential_scope"));
            }
            credentials
                .route_execution_token(&record.hash, &scope, &record.execution_id)
                .await
                .map_err(|_| StoreError::Backend("d1.token_route"))?;
        }
    }
    Ok(())
}

/// An execution session on this edition: the object's session, with the
/// same D1 step before its commit as a plain commit ([`before_commit`]).
pub struct CfSession<D: D1, O: Objects, B: ObjectBucket> {
    inner: <O::Handle as ObjectHandle>::Session,
    store: CfStore<D, O, B>,
}
impl<D: D1, O: Objects, B: ObjectBucket> ExecutionStore for CfSession<D, O, B> {
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        self.inner.load(id).await
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        let store = &self.store;
        let id = batch.execution_id.clone();
        before_commit(&store.scope, &store.control(), &store.memory, &batch).await?;
        let result = self.inner.commit(batch).await?;
        if store.settle_commits && matches!(result, CommitResult::Committed(_)) {
            store.settle_after(&id).await;
        }
        Ok(result)
    }
}
impl<D: D1, O: Objects, B: ObjectBucket> runbook_core::session::ExecutionSession
    for CfSession<D, O, B>
{
    fn transaction_time_ms(&self) -> i64 {
        self.inner.transaction_time_ms()
    }
    async fn actor_replay(
        &self,
        prefix: &str,
    ) -> Result<Option<runbook_core::IdempotencyRecord>, StoreError> {
        self.inner.actor_replay(prefix).await
    }
    async fn release(&self) {
        self.inner.release().await;
    }
}
