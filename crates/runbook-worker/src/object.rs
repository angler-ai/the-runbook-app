//! CF5: one run's ExecutionDO store as a host reaches it.
//!
//! Inside the run's own object the store is local (`DoStore`, synchronous
//! SQL). Every other host reaches it through the object protocol
//! (`protocol.rs`): the api Worker's cron (archives and retention), another
//! run's object (`cancel_previous` notices, reruns) and the conformance
//! harness (one Durable Object per run). [`ObjectHandle`] is what the
//! composed store (`compose.rs`) needs from either.
use runbook_core::{
    ClaimWrite, CommitResult, ExecutionCommit, ExecutionStore, IdempotencyRecord, StoreError,
    StoredExecution,
    actor::Replay,
    catalog::DerivedUpdate,
    claims::ClaimStore,
    credentials::SecretHash,
    execution_credentials::{
        CredentialFamily, ExecutionCredentialRecord, ExecutionCredentialStore,
    },
    retention::WorkState,
    session::{ExecutionSession, SessionStore},
    start::StartRecord,
    work_queue::{WorkId, WorkLease, WorkQueue},
};
use runbook_model::{ActorRef, ClaimRef, ExecutionId, StepRunId, TokenId};
use runbook_store_do::{
    DoSql, DoStore, OutboxStream, StartInit,
    logs::{LogChunk, PendingChunk},
};
use std::future::Future;

/// One run's object, bound to the caller's tenant (§18: the scope comes
/// from a verified credential; the object refuses any other).
pub trait ObjectHandle: Clone + 'static {
    type Session: ExecutionSession;
    /// The object's store clock.
    fn now_ms(&self) -> i64;
    fn load(&self, id: &ExecutionId) -> impl Future<Output = Result<StoredExecution, StoreError>>;
    fn commit(
        &self,
        batch: ExecutionCommit,
    ) -> impl Future<Output = Result<CommitResult, StoreError>>;
    fn begin(&self, id: &ExecutionId) -> impl Future<Output = Result<Self::Session, StoreError>>;

    fn heartbeat(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        replay: Option<&Replay>,
    ) -> impl Future<Output = Result<Option<i64>, StoreError>>;
    fn append_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        lines: Vec<String>,
        replay: Option<&Replay>,
    ) -> impl Future<Output = Result<u64, StoreError>>;
    fn claim_record(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> impl Future<Output = Result<Option<ClaimWrite>, StoreError>>;
    fn read_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<(u64, String)>, StoreError>>;

    fn credential_family(
        &self,
        family_id: &str,
    ) -> impl Future<Output = Result<Option<CredentialFamily>, StoreError>>;
    fn credential_by_hash(
        &self,
        hash: &SecretHash,
    ) -> impl Future<Output = Result<Option<ExecutionCredentialRecord>, StoreError>>;
    fn credential_by_id(
        &self,
        token_id: &TokenId,
    ) -> impl Future<Output = Result<Option<ExecutionCredentialRecord>, StoreError>>;
    fn family_by_idempotency_key(
        &self,
        id: &ExecutionId,
        key: &str,
    ) -> impl Future<Output = Result<Option<CredentialFamily>, StoreError>>;

    fn claim_due(
        &self,
        limit: usize,
        lease_ms: u64,
    ) -> impl Future<Output = Result<Vec<WorkLease>, StoreError>>;
    fn ack(&self, id: &WorkId, generation: u64) -> impl Future<Output = Result<(), StoreError>>;
    /// Lease due work for the object's own settle: everything but
    /// notification intents, which leave through the queue publisher.
    fn claim_settle_work(
        &self,
        limit: usize,
        lease_ms: u64,
    ) -> impl Future<Output = Result<Vec<WorkLease>, StoreError>>;
    /// The run this object holds for the caller's tenant, if any.
    fn current_execution(&self) -> impl Future<Output = Result<Option<ExecutionId>, StoreError>>;

    fn pending_derived(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<(u64, DerivedUpdate)>, StoreError>>;
    fn derived_row(
        &self,
        sequence: u64,
    ) -> impl Future<Output = Result<Option<DerivedUpdate>, StoreError>>;
    fn ack_derived(&self, through: u64) -> impl Future<Output = Result<u64, StoreError>>;
    fn defer_outbox(&self, stream: OutboxStream) -> impl Future<Output = Result<i64, StoreError>>;

    fn initialize_start(&self, init: StartInit) -> impl Future<Output = Result<(), StoreError>>;
    fn admit(
        &self,
        id: &ExecutionId,
        admitted_at_ms: i64,
    ) -> impl Future<Output = Result<(), StoreError>>;
    fn start_record(
        &self,
        id: &ExecutionId,
    ) -> impl Future<Output = Result<Option<StartRecord>, StoreError>>;

    /// The run-owned rows a retention export lists beside the run.
    fn export_parts(
        &self,
        id: &ExecutionId,
    ) -> impl Future<Output = Result<(Vec<IdempotencyRecord>, Vec<WorkState>), StoreError>>;
    /// CF12 review round 1: everything a portable export needs from the
    /// run's object, in one request.
    fn export_run(
        &self,
        id: &ExecutionId,
    ) -> impl Future<Output = Result<runbook_store_do::portable::RunExport, StoreError>>;
    /// Empty the object after its deletion committed in D1.
    fn purge(&self, id: &ExecutionId) -> impl Future<Output = Result<bool, StoreError>>;
    /// CF8: write a whole run into this empty object (a portable import).
    fn install(
        &self,
        id: &ExecutionId,
        unit: runbook_store_do::portable::RunInstall,
    ) -> impl Future<Output = Result<(), StoreError>>;
    /// Gate fix D2: release a run an import installed held; true when it
    /// was held.
    fn release(&self, id: &ExecutionId) -> impl Future<Output = Result<bool, StoreError>>;
    /// CF5 step logs: the next full chunk to move to R2, recording one that
    /// is there, and the chunks holding lines after `after`.
    fn log_flush(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> impl Future<Output = Result<Option<PendingChunk>, StoreError>>;
    fn log_flushed(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        chunk: &LogChunk,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    fn log_chunks(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
    ) -> impl Future<Output = Result<Vec<LogChunk>, StoreError>>;
    /// The conformance `create`: the run's head at revision 0.
    fn create_execution(
        &self,
        projection: runbook_core::ExecutionProjection,
    ) -> impl Future<Output = Result<(), StoreError>>;
}

/// The local handle: this object's own storage.
impl<S: DoSql> ObjectHandle for DoStore<S> {
    type Session = runbook_store_do::DoSession<S>;
    fn now_ms(&self) -> i64 {
        DoStore::now_ms(self)
    }
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        ExecutionStore::load(self, id).await
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        ExecutionStore::commit(self, batch).await
    }
    async fn begin(&self, id: &ExecutionId) -> Result<Self::Session, StoreError> {
        SessionStore::begin(self, id).await
    }
    async fn heartbeat(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        replay: Option<&Replay>,
    ) -> Result<Option<i64>, StoreError> {
        ClaimStore::heartbeat(self, id, step_run_id, claim, owner, replay).await
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
        ClaimStore::append_logs(self, id, step_run_id, claim, owner, lines, replay).await
    }
    async fn claim_record(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<Option<ClaimWrite>, StoreError> {
        ClaimStore::claim_record(self, id, step_run_id).await
    }
    async fn read_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        ClaimStore::read_logs(self, id, step_run_id, after, limit).await
    }
    async fn credential_family(
        &self,
        family_id: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        ExecutionCredentialStore::credential_family(self, family_id).await
    }
    async fn credential_by_hash(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        ExecutionCredentialStore::credential_by_hash(self, hash).await
    }
    async fn credential_by_id(
        &self,
        token_id: &TokenId,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        ExecutionCredentialStore::credential_by_id(self, token_id).await
    }
    async fn family_by_idempotency_key(
        &self,
        id: &ExecutionId,
        key: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        ExecutionCredentialStore::family_by_idempotency_key(self, id, key).await
    }
    async fn claim_due(&self, limit: usize, lease_ms: u64) -> Result<Vec<WorkLease>, StoreError> {
        WorkQueue::claim_due(self, limit, lease_ms).await
    }
    async fn ack(&self, id: &WorkId, generation: u64) -> Result<(), StoreError> {
        WorkQueue::ack(self, id, generation).await
    }
    async fn claim_settle_work(
        &self,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<WorkLease>, StoreError> {
        self.claim_due_of(limit, lease_ms, |kind| {
            !matches!(
                kind,
                runbook_core::work_queue::WorkKind::Notification { .. }
            )
        })
    }
    async fn current_execution(&self) -> Result<Option<ExecutionId>, StoreError> {
        DoStore::current_execution(self)
    }
    async fn pending_derived(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<(u64, DerivedUpdate)>, StoreError> {
        DoStore::pending_derived(self, after, limit)
    }
    async fn derived_row(&self, sequence: u64) -> Result<Option<DerivedUpdate>, StoreError> {
        DoStore::derived_row(self, sequence)
    }
    async fn ack_derived(&self, through: u64) -> Result<u64, StoreError> {
        DoStore::ack_derived(self, through)
    }
    async fn defer_outbox(&self, stream: OutboxStream) -> Result<i64, StoreError> {
        DoStore::defer_outbox(self, stream)
    }
    async fn initialize_start(&self, init: StartInit) -> Result<(), StoreError> {
        DoStore::initialize_start(self, init)
    }
    async fn admit(&self, id: &ExecutionId, admitted_at_ms: i64) -> Result<(), StoreError> {
        DoStore::admit(self, id, admitted_at_ms)
    }
    async fn start_record(&self, id: &ExecutionId) -> Result<Option<StartRecord>, StoreError> {
        DoStore::start_record(self, id)
    }
    async fn export_parts(
        &self,
        id: &ExecutionId,
    ) -> Result<(Vec<IdempotencyRecord>, Vec<WorkState>), StoreError> {
        DoStore::export_parts(self, id)
    }
    async fn export_run(
        &self,
        id: &ExecutionId,
    ) -> Result<runbook_store_do::portable::RunExport, StoreError> {
        DoStore::export_run(self, id)
    }
    async fn purge(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        DoStore::purge(self, id)
    }
    async fn install(
        &self,
        id: &ExecutionId,
        unit: runbook_store_do::portable::RunInstall,
    ) -> Result<(), StoreError> {
        if &unit.run.projection.execution_id != id {
            return Err(StoreError::InvalidBatch("do.object_mismatch"));
        }
        DoStore::install(self, unit)
    }
    async fn release(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        DoStore::release(self, id)
    }
    async fn log_flush(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<Option<PendingChunk>, StoreError> {
        DoStore::log_flush(self, id, step_run_id)
    }
    async fn log_flushed(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        chunk: &LogChunk,
    ) -> Result<bool, StoreError> {
        DoStore::log_flushed(self, id, step_run_id, chunk)
    }
    async fn log_chunks(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
    ) -> Result<Vec<LogChunk>, StoreError> {
        DoStore::log_chunks(self, id, step_run_id, after)
    }
    async fn create_execution(
        &self,
        projection: runbook_core::ExecutionProjection,
    ) -> Result<(), StoreError> {
        DoStore::create_execution(self, projection)
    }
}
