//! CF5: the object protocol, one run's ExecutionDO store over a request.
//!
//! A host that is not the run's own object (the api Worker's cron for
//! archives and retention, another run's object, the conformance harness)
//! reaches it with [`RemoteObject`]: each [`ObjectHandle`] call is one
//! [`Request`] (the run, the caller's tenant and an [`Op`]) answered by
//! [`serve`] inside the object, over the store the object opens for that
//! tenant (§18: it refuses any other, exactly as locally). The transport is
//! a Durable Object `fetch` on Workers and an in-process call in native
//! tests, so both run the same encoding.
//!
//! Sessions: a local session holds the object's gate from `begin` to its
//! commit. Over the protocol `begin` and `commit` are two requests, so a
//! remote session is optimistic instead: it carries the object's write
//! fence ([`Fence`]) and conflicts if anything wrote in between.
use crate::object::ObjectHandle;
use runbook_core::{
    ClaimWrite, CommitResult, ExecutionCommit, ExecutionStore, IdempotencyRecord, StoreError,
    StoredExecution, WriteStage,
    actor::Replay,
    catalog::{DerivedUpdate, TenantScope},
    claims::ClaimStore,
    credentials::SecretHash,
    execution_credentials::{
        CredentialFamily, ExecutionCredentialRecord, ExecutionCredentialStore,
    },
    retention::WorkState,
    session::ExecutionSession,
    start::StartRecord,
    work_queue::{WorkId, WorkLease, WorkQueue},
};
use runbook_model::{ActorRef, ClaimRef, ExecutionId, StepRunId, TokenId};
use runbook_store_do::{
    DoSql, DoStore, Fence, FenceRef, OutboxStream, StartInit,
    logs::{LogChunk, PendingChunk},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    future::Future,
    rc::Rc,
};

/// One operation on a run's object. Externally tagged (`{"load": null}`,
/// `{"commit": {…}}`): an internally tagged enum buffers its fields, and
/// with `serde_json`'s `arbitrary_precision` buffered numbers no longer
/// read back as integers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Load,
    /// `fault` is a test seam: the object fails the commit after `stage`.
    Commit {
        batch: ExecutionCommit,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fault: Option<WriteStage>,
    },
    Begin,
    /// A commit with the whole fence (its snapshot included), as a release
    /// before CF13 review round 1 sends it; still served.
    CommitFenced {
        batch: ExecutionCommit,
        fence: Fence,
    },
    /// A commit with only what the fence checks (CF13 review round 1).
    CommitFencedRef {
        batch: ExecutionCommit,
        fence: FenceRef,
    },
    ReplayAt {
        prefix: String,
        at_ms: i64,
    },
    Heartbeat {
        step_run_id: StepRunId,
        claim: ClaimRef,
        owner: ActorRef,
        replay: Option<Replay>,
    },
    AppendLogs {
        step_run_id: StepRunId,
        claim: ClaimRef,
        owner: ActorRef,
        lines: Vec<String>,
        replay: Option<Replay>,
    },
    ClaimRecord {
        step_run_id: StepRunId,
    },
    ReadLogs {
        step_run_id: StepRunId,
        after: u64,
        limit: usize,
    },
    CredentialFamily {
        family_id: String,
    },
    CredentialByHash {
        hash: SecretHash,
    },
    CredentialById {
        token_id: TokenId,
    },
    FamilyByIdempotencyKey {
        key: String,
    },
    ClaimDue {
        limit: usize,
        lease_ms: u64,
    },
    Ack {
        work: WorkId,
        generation: u64,
    },
    ClaimSettleWork {
        limit: usize,
        lease_ms: u64,
    },
    CurrentExecution,
    PendingDerived {
        after: Option<u64>,
        limit: usize,
    },
    DerivedRow {
        sequence: u64,
    },
    AckDerived {
        through: u64,
    },
    DeferOutbox {
        stream: OutboxStream,
    },
    InitializeStart {
        init: StartInit,
    },
    Admit {
        admitted_at_ms: i64,
    },
    StartRecord,
    ExportParts,
    /// CF12 review round 1: the run as a portable export reads it.
    ExportRun,
    Purge,
    /// CF8: write a whole run from a portable archive into this empty
    /// object.
    Install {
        unit: Box<runbook_store_do::portable::RunInstall>,
    },
    /// Gate fix D2: release a run an import installed held.
    Release,
    LogFlush {
        step_run_id: StepRunId,
    },
    LogFlushed {
        step_run_id: StepRunId,
        chunk: LogChunk,
    },
    LogChunks {
        step_run_id: StepRunId,
        after: u64,
    },
    /// The object's store clock.
    Now,
    /// Test seams (the conformance harness only).
    CreateExecution {
        projection: runbook_core::ExecutionProjection,
    },
    ExpireLease {
        lease: WorkLease,
    },
    TableCounts,
}

/// One request: the run, the caller's verified tenant and the operation.
/// `clock` is honored only by test hosts (the conformance harness's shared
/// test clock); a production object ignores it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub execution_id: ExecutionId,
    pub scope: TenantScope,
    pub op: Op,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<i64>,
}

/// The answer to a [`Request`]: a JSON value, or a store error.
pub type Reply = Result<Value, StoreError>;

/// The wire form of a reply (`{"ok": …}` or `{"error": {kind, code}}`).
pub fn encode_reply(reply: &Reply) -> Value {
    match reply {
        Ok(value) => json!({ "ok": value }),
        Err(StoreError::NotFound) => json!({ "error": { "kind": "not_found" } }),
        Err(StoreError::InvalidBatch(code)) => {
            json!({ "error": { "kind": "invalid", "code": code } })
        }
        Err(StoreError::Backend(code)) => json!({ "error": { "kind": "backend", "code": code } }),
    }
}

/// [`encode_reply`], moving the value instead of copying it (CF13 review
/// round 1: a run's snapshot is the largest reply, and a copy of it was
/// the object's largest allocation).
pub fn into_wire(reply: Reply) -> Value {
    match reply {
        Ok(value) => {
            let mut wire = serde_json::Map::new();
            wire.insert("ok".into(), value);
            Value::Object(wire)
        }
        Err(_) => encode_reply(&reply),
    }
}

/// Read a wire reply back. Error codes are interned, so a remote code is a
/// `&'static str` like a local one (codes are a small fixed set).
pub fn decode_reply(mut wire: Value) -> Reply {
    if let Some(ok) = wire.get_mut("ok") {
        return Ok(ok.take());
    }
    let error = wire
        .get("error")
        .ok_or(StoreError::Backend("object.bad_reply"))?;
    let code = error.get("code").and_then(Value::as_str).unwrap_or("");
    Err(match error.get("kind").and_then(Value::as_str) {
        Some("not_found") => StoreError::NotFound,
        Some("invalid") => StoreError::InvalidBatch(intern(code)),
        Some("backend") => StoreError::Backend(intern(code)),
        _ => StoreError::Backend("object.bad_reply"),
    })
}

thread_local! {
    static CODES: RefCell<BTreeSet<&'static str>> = RefCell::default();
}
fn intern(code: &str) -> &'static str {
    CODES.with(|codes| {
        let mut codes = codes.borrow_mut();
        if let Some(known) = codes.get(code) {
            return *known;
        }
        let leaked: &'static str = Box::leak(code.to_owned().into_boxed_str());
        codes.insert(leaked);
        leaked
    })
}

fn to_value<T: Serialize>(value: T) -> Reply {
    serde_json::to_value(value).map_err(|_| StoreError::Backend("object.encode"))
}

/// Answer `op` on `store`, the object's store opened for the request's run
/// and tenant. Test seams (`ExpireLease`, `TableCounts`, a commit fault) are
/// served only when `test_hooks` is set.
pub async fn serve<S: DoSql>(store: &DoStore<S>, op: Op, test_hooks: bool) -> Reply {
    let id = store.name().clone();
    match op {
        Op::Load => to_value(ExecutionStore::load(store, &id).await?),
        Op::Commit { batch, fault } => {
            if let Some(stage) = fault {
                if !test_hooks {
                    return Err(StoreError::Backend("object.test_hooks_off"));
                }
                store.fail_after(stage);
            }
            to_value(ExecutionStore::commit(store, batch).await?)
        }
        Op::Begin => to_value(store.begin_fenced(&id)?),
        Op::CommitFenced { batch, fence } => to_value(store.commit_fenced(batch, &fence)?),
        Op::CommitFencedRef { batch, fence } => to_value(store.commit_fenced_ref(batch, &fence)?),
        Op::ReplayAt { prefix, at_ms } => to_value(store.replay_at(&id, &prefix, at_ms)?),
        Op::Heartbeat {
            step_run_id,
            claim,
            owner,
            replay,
        } => to_value(
            ClaimStore::heartbeat(store, &id, &step_run_id, &claim, &owner, replay.as_ref())
                .await?,
        ),
        Op::AppendLogs {
            step_run_id,
            claim,
            owner,
            lines,
            replay,
        } => to_value(
            ClaimStore::append_logs(
                store,
                &id,
                &step_run_id,
                &claim,
                &owner,
                lines,
                replay.as_ref(),
            )
            .await?,
        ),
        Op::ClaimRecord { step_run_id } => {
            to_value(ClaimStore::claim_record(store, &id, &step_run_id).await?)
        }
        Op::ReadLogs {
            step_run_id,
            after,
            limit,
        } => to_value(ClaimStore::read_logs(store, &id, &step_run_id, after, limit).await?),
        Op::CredentialFamily { family_id } => {
            to_value(ExecutionCredentialStore::credential_family(store, &family_id).await?)
        }
        Op::CredentialByHash { hash } => {
            to_value(ExecutionCredentialStore::credential_by_hash(store, &hash).await?)
        }
        Op::CredentialById { token_id } => {
            to_value(ExecutionCredentialStore::credential_by_id(store, &token_id).await?)
        }
        Op::FamilyByIdempotencyKey { key } => {
            to_value(ExecutionCredentialStore::family_by_idempotency_key(store, &id, &key).await?)
        }
        Op::ClaimDue { limit, lease_ms } => {
            to_value(WorkQueue::claim_due(store, limit, lease_ms).await?)
        }
        Op::Ack { work, generation } => to_value(WorkQueue::ack(store, &work, generation).await?),
        Op::ClaimSettleWork { limit, lease_ms } => {
            to_value(ObjectHandle::claim_settle_work(store, limit, lease_ms).await?)
        }
        Op::CurrentExecution => to_value(store.current_execution()?),
        Op::PendingDerived { after, limit } => to_value(store.pending_derived(after, limit)?),
        Op::DerivedRow { sequence } => to_value(store.derived_row(sequence)?),
        Op::AckDerived { through } => to_value(store.ack_derived(through)?),
        Op::DeferOutbox { stream } => to_value(store.defer_outbox(stream)?),
        Op::InitializeStart { init } => to_value(store.initialize_start(init)?),
        Op::Admit { admitted_at_ms } => to_value(store.admit(&id, admitted_at_ms)?),
        Op::StartRecord => to_value(store.start_record(&id)?),
        Op::ExportParts => to_value(store.export_parts(&id)?),
        Op::ExportRun => to_value(store.export_run(&id)?),
        Op::Purge => to_value(store.purge(&id)?),
        Op::Install { unit } => to_value(store.install(*unit)?),
        Op::Release => to_value(store.release(&id)?),
        Op::LogFlush { step_run_id } => to_value(store.log_flush(&id, &step_run_id)?),
        Op::LogFlushed { step_run_id, chunk } => {
            to_value(store.log_flushed(&id, &step_run_id, &chunk)?)
        }
        Op::LogChunks { step_run_id, after } => {
            to_value(store.log_chunks(&id, &step_run_id, after)?)
        }
        Op::Now => to_value(store.now_ms()),
        Op::CreateExecution { projection } if test_hooks => {
            to_value(store.create_execution(projection)?)
        }
        Op::ExpireLease { lease } if test_hooks => to_value(store.expire_lease(&lease)?),
        Op::TableCounts if test_hooks => to_value(store.table_counts()?),
        Op::CreateExecution { .. } | Op::ExpireLease { .. } | Op::TableCounts => {
            Err(StoreError::Backend("object.test_hooks_off"))
        }
    }
}

/// How a host sends a [`Request`] to the run's object and reads the reply.
pub trait Transport: Clone + 'static {
    fn call(&self, request: Request) -> impl Future<Output = Reply>;
    /// CF6: a request to an organization's OrgDO.
    fn call_org(&self, request: crate::quota::QuotaRequest) -> impl Future<Output = Reply>;
    /// CF11: a request to a webhook subscription's WebhookDO.
    fn call_webhook(&self, request: crate::webhooks::WebhookRequest)
    -> impl Future<Output = Reply>;
    /// The clock a test host stamps on each request (`None` in production).
    fn clock(&self) -> Option<i64> {
        None
    }
}

/// A run's object reached over a [`Transport`].
#[derive(Clone)]
pub struct RemoteObject<T: Transport> {
    transport: T,
    id: ExecutionId,
    scope: TenantScope,
    /// A test fault the next commit carries.
    fault: Rc<Cell<Option<WriteStage>>>,
}
impl<T: Transport> RemoteObject<T> {
    pub fn new(transport: T, id: ExecutionId, scope: TenantScope) -> Self {
        Self {
            transport,
            id,
            scope,
            fault: Rc::default(),
        }
    }
    /// Test seam: fail the next commit after `stage`.
    pub fn fail_next_commit(&self, stage: WriteStage) {
        self.fault.set(Some(stage));
    }
    pub async fn call<R: DeserializeOwned>(&self, op: Op) -> Result<R, StoreError> {
        let value = self
            .transport
            .call(Request {
                execution_id: self.id.clone(),
                scope: self.scope.clone(),
                op,
                clock: self.transport.clock(),
            })
            .await?;
        serde_json::from_value(value).map_err(|_| StoreError::Backend("object.decode"))
    }
    /// The run's own ID (calls for another run are refused locally, as the
    /// object would refuse them).
    fn own(&self, id: &ExecutionId) -> Result<(), StoreError> {
        if id == &self.id {
            Ok(())
        } else {
            Err(StoreError::NotFound)
        }
    }
    /// Test seam: expire a lease the object granted.
    pub async fn expire_lease(&self, lease: &WorkLease) -> Result<(), StoreError> {
        self.call(Op::ExpireLease {
            lease: lease.clone(),
        })
        .await
    }
    /// Test seam: rows per table in the object.
    pub async fn table_counts(&self) -> Result<Vec<(String, u64)>, StoreError> {
        self.call(Op::TableCounts).await
    }
}

/// A session over the protocol (see the module docs).
///
/// CF13 review round 1: the session hands its snapshot to the first
/// `load` instead of a copy (a second `load` reads the object again; the
/// commit is fenced by the begin's revision and writes either way), and
/// its commit sends only what the fence checks ([`FenceRef`]), not the
/// snapshot back.
pub struct RemoteSession<T: Transport> {
    object: RemoteObject<T>,
    fence: FenceRef,
    snapshot: RefCell<Option<StoredExecution>>,
    consumed: Cell<bool>,
}
impl<T: Transport> ExecutionStore for RemoteSession<T> {
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        if id != &self.fence.execution_id {
            return Err(StoreError::NotFound);
        }
        if self.consumed.get() {
            return Err(StoreError::InvalidBatch("store.session_closed"));
        }
        let taken = self.snapshot.borrow_mut().take();
        match taken {
            Some(snapshot) => Ok(snapshot),
            None => self.object.call(Op::Load).await,
        }
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        if self.consumed.replace(true) {
            return Err(StoreError::InvalidBatch("store.session_closed"));
        }
        self.snapshot.borrow_mut().take();
        self.object
            .call(Op::CommitFencedRef {
                batch,
                fence: self.fence.clone(),
            })
            .await
    }
}
impl<T: Transport> ExecutionSession for RemoteSession<T> {
    fn transaction_time_ms(&self) -> i64 {
        self.fence.at_ms
    }
    async fn actor_replay(&self, prefix: &str) -> Result<Option<IdempotencyRecord>, StoreError> {
        if self.consumed.get() {
            return Err(StoreError::InvalidBatch("store.session_closed"));
        }
        self.object
            .call(Op::ReplayAt {
                prefix: prefix.to_owned(),
                at_ms: self.fence.at_ms,
            })
            .await
    }
    async fn release(&self) {
        self.consumed.set(true);
    }
}

impl<T: Transport> ObjectHandle for RemoteObject<T> {
    type Session = RemoteSession<T>;
    fn now_ms(&self) -> i64 {
        self.transport.clock().unwrap_or_else(wall_clock_ms)
    }
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        self.own(id)?;
        self.call(Op::Load).await
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        self.own(&batch.execution_id)?;
        let fault = self.fault.take();
        self.call(Op::Commit { batch, fault }).await
    }
    async fn begin(&self, id: &ExecutionId) -> Result<Self::Session, StoreError> {
        self.own(id)?;
        let fence: Fence = self.call(Op::Begin).await?;
        Ok(RemoteSession {
            object: self.clone(),
            fence: fence.to_ref(),
            snapshot: RefCell::new(Some(fence.snapshot)),
            consumed: Cell::new(false),
        })
    }
    async fn heartbeat(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        replay: Option<&Replay>,
    ) -> Result<Option<i64>, StoreError> {
        self.own(id)?;
        self.call(Op::Heartbeat {
            step_run_id: step_run_id.clone(),
            claim: claim.clone(),
            owner: owner.clone(),
            replay: replay.cloned(),
        })
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
        self.own(id)?;
        self.call(Op::AppendLogs {
            step_run_id: step_run_id.clone(),
            claim: claim.clone(),
            owner: owner.clone(),
            lines,
            replay: replay.cloned(),
        })
        .await
    }
    async fn claim_record(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<Option<ClaimWrite>, StoreError> {
        self.own(id)?;
        self.call(Op::ClaimRecord {
            step_run_id: step_run_id.clone(),
        })
        .await
    }
    async fn read_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        self.own(id)?;
        self.call(Op::ReadLogs {
            step_run_id: step_run_id.clone(),
            after,
            limit,
        })
        .await
    }
    async fn credential_family(
        &self,
        family_id: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        self.call(Op::CredentialFamily {
            family_id: family_id.to_owned(),
        })
        .await
    }
    async fn credential_by_hash(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        self.call(Op::CredentialByHash { hash: *hash }).await
    }
    async fn credential_by_id(
        &self,
        token_id: &TokenId,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        self.call(Op::CredentialById {
            token_id: token_id.clone(),
        })
        .await
    }
    async fn family_by_idempotency_key(
        &self,
        id: &ExecutionId,
        key: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        self.own(id)?;
        self.call(Op::FamilyByIdempotencyKey {
            key: key.to_owned(),
        })
        .await
    }
    async fn claim_due(&self, limit: usize, lease_ms: u64) -> Result<Vec<WorkLease>, StoreError> {
        self.call(Op::ClaimDue { limit, lease_ms }).await
    }
    async fn ack(&self, id: &WorkId, generation: u64) -> Result<(), StoreError> {
        self.own(&id.execution_id)?;
        self.call(Op::Ack {
            work: id.clone(),
            generation,
        })
        .await
    }
    async fn claim_settle_work(
        &self,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<WorkLease>, StoreError> {
        self.call(Op::ClaimSettleWork { limit, lease_ms }).await
    }
    async fn current_execution(&self) -> Result<Option<ExecutionId>, StoreError> {
        self.call(Op::CurrentExecution).await
    }
    async fn pending_derived(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<(u64, DerivedUpdate)>, StoreError> {
        self.call(Op::PendingDerived { after, limit }).await
    }
    async fn derived_row(&self, sequence: u64) -> Result<Option<DerivedUpdate>, StoreError> {
        self.call(Op::DerivedRow { sequence }).await
    }
    async fn ack_derived(&self, through: u64) -> Result<u64, StoreError> {
        self.call(Op::AckDerived { through }).await
    }
    async fn defer_outbox(&self, stream: OutboxStream) -> Result<i64, StoreError> {
        self.call(Op::DeferOutbox { stream }).await
    }
    async fn initialize_start(&self, init: StartInit) -> Result<(), StoreError> {
        self.own(&init.projection.execution_id)?;
        self.call(Op::InitializeStart { init }).await
    }
    async fn admit(&self, id: &ExecutionId, admitted_at_ms: i64) -> Result<(), StoreError> {
        self.own(id)?;
        self.call(Op::Admit { admitted_at_ms }).await
    }
    async fn start_record(&self, id: &ExecutionId) -> Result<Option<StartRecord>, StoreError> {
        self.own(id)?;
        self.call(Op::StartRecord).await
    }
    async fn export_parts(
        &self,
        id: &ExecutionId,
    ) -> Result<(Vec<IdempotencyRecord>, Vec<WorkState>), StoreError> {
        self.own(id)?;
        self.call(Op::ExportParts).await
    }
    async fn export_run(
        &self,
        id: &ExecutionId,
    ) -> Result<runbook_store_do::portable::RunExport, StoreError> {
        self.own(id)?;
        self.call(Op::ExportRun).await
    }
    async fn purge(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        self.own(id)?;
        self.call(Op::Purge).await
    }
    async fn install(
        &self,
        id: &ExecutionId,
        unit: runbook_store_do::portable::RunInstall,
    ) -> Result<(), StoreError> {
        self.own(id)?;
        self.call(Op::Install {
            unit: Box::new(unit),
        })
        .await
    }
    async fn release(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        self.own(id)?;
        self.call(Op::Release).await
    }
    async fn log_flush(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<Option<PendingChunk>, StoreError> {
        self.own(id)?;
        self.call(Op::LogFlush {
            step_run_id: step_run_id.clone(),
        })
        .await
    }
    async fn log_flushed(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        chunk: &LogChunk,
    ) -> Result<bool, StoreError> {
        self.own(id)?;
        self.call(Op::LogFlushed {
            step_run_id: step_run_id.clone(),
            chunk: chunk.clone(),
        })
        .await
    }
    async fn log_chunks(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
    ) -> Result<Vec<LogChunk>, StoreError> {
        self.own(id)?;
        self.call(Op::LogChunks {
            step_run_id: step_run_id.clone(),
            after,
        })
        .await
    }
    async fn create_execution(
        &self,
        projection: runbook_core::ExecutionProjection,
    ) -> Result<(), StoreError> {
        self.own(&projection.execution_id)?;
        self.call(Op::CreateExecution { projection }).await
    }
}

/// Objects reached over a transport. `tracked` lists every object handed
/// out (test hosts, where `claim_due` sweeps them all); production leaves
/// it off: the api Worker reaches objects by ID and lists none.
#[derive(Clone)]
pub struct RemoteObjects<T: Transport> {
    transport: T,
    tracked: Option<Rc<RefCell<BTreeSet<ExecutionId>>>>,
    /// A test fault for the next commit (moved onto its object) or for a
    /// run deletion (taken by the composed store).
    fault: Rc<Cell<Option<WriteStage>>>,
}
impl<T: Transport> RemoteObjects<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            tracked: None,
            fault: Rc::default(),
        }
    }
    /// A test host's objects: every handle is remembered for `reachable`.
    pub fn tracked(transport: T) -> Self {
        Self {
            tracked: Some(Rc::default()),
            ..Self::new(transport)
        }
    }
    pub fn transport(&self) -> &T {
        &self.transport
    }
    /// Test seam: arm a fault for the next commit or run deletion.
    pub fn arm_fault(&self, stage: WriteStage) {
        self.fault.set(Some(stage));
    }
}
impl<T: Transport> crate::compose::Objects for RemoteObjects<T> {
    type Handle = RemoteObject<T>;
    type Quota = crate::quota::RemoteQuota<T>;
    fn quota(&self, org: &runbook_model::OrganizationId) -> Self::Quota {
        crate::quota::RemoteQuota::new(self.transport.clone(), org.clone())
    }
    type Webhooks = crate::webhooks::ViaTransport<T>;
    fn webhooks(&self) -> Self::Webhooks {
        crate::webhooks::ViaTransport(self.transport.clone())
    }
    fn object(&self, id: &ExecutionId, scope: &TenantScope) -> Result<Self::Handle, StoreError> {
        if let Some(tracked) = &self.tracked {
            tracked.borrow_mut().insert(id.clone());
        }
        Ok(RemoteObject::new(
            self.transport.clone(),
            id.clone(),
            scope.clone(),
        ))
    }
    fn reachable(&self) -> Vec<ExecutionId> {
        self.tracked
            .as_ref()
            .map(|t| t.borrow().iter().cloned().collect())
            .unwrap_or_default()
    }
    fn before_commit(&self, object: &Self::Handle) {
        match self.fault.get() {
            Some(WriteStage::RunDeletion) | None => {}
            Some(stage) => {
                self.fault.set(None);
                object.fail_next_commit(stage);
            }
        }
    }
    fn take_fault(&self, stage: WriteStage) -> bool {
        let hit = self.fault.get() == Some(stage);
        if hit {
            self.fault.set(None);
        }
        hit
    }
}

#[cfg(target_arch = "wasm32")]
fn wall_clock_ms() -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    {
        js_sys::Date::now() as i64
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn wall_clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// A run's object as an ExecutionDO reaches it: its own run's storage
/// locally, any other run's over the protocol (CF5: a start that evicts
/// holders, a rerun's new run, notices for other runs).
#[derive(Clone)]
pub enum Hybrid<S: DoSql, T: Transport> {
    Local(DoStore<S>),
    Remote(RemoteObject<T>),
}

/// The session of a [`Hybrid`] handle.
pub enum HybridSession<S: DoSql, T: Transport> {
    Local(runbook_store_do::DoSession<S>),
    Remote(RemoteSession<T>),
}
impl<S: DoSql, T: Transport> ExecutionStore for HybridSession<S, T> {
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        match self {
            Self::Local(s) => s.load(id).await,
            Self::Remote(s) => s.load(id).await,
        }
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        match self {
            Self::Local(s) => s.commit(batch).await,
            Self::Remote(s) => s.commit(batch).await,
        }
    }
}
impl<S: DoSql, T: Transport> ExecutionSession for HybridSession<S, T> {
    fn transaction_time_ms(&self) -> i64 {
        match self {
            Self::Local(s) => s.transaction_time_ms(),
            Self::Remote(s) => s.transaction_time_ms(),
        }
    }
    async fn actor_replay(&self, prefix: &str) -> Result<Option<IdempotencyRecord>, StoreError> {
        match self {
            Self::Local(s) => s.actor_replay(prefix).await,
            Self::Remote(s) => s.actor_replay(prefix).await,
        }
    }
    async fn release(&self) {
        match self {
            Self::Local(s) => s.release().await,
            Self::Remote(s) => s.release().await,
        }
    }
}

macro_rules! hybrid {
    ($self:ident . $method:ident ( $($arg:expr),* )) => {
        match $self {
            Hybrid::Local(h) => ObjectHandle::$method(h, $($arg),*).await,
            Hybrid::Remote(h) => ObjectHandle::$method(h, $($arg),*).await,
        }
    };
}

impl<S: DoSql, T: Transport> ObjectHandle for Hybrid<S, T> {
    type Session = HybridSession<S, T>;
    fn now_ms(&self) -> i64 {
        match self {
            Self::Local(h) => ObjectHandle::now_ms(h),
            Self::Remote(h) => ObjectHandle::now_ms(h),
        }
    }
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        hybrid!(self.load(id))
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        hybrid!(self.commit(batch))
    }
    async fn begin(&self, id: &ExecutionId) -> Result<Self::Session, StoreError> {
        Ok(match self {
            Self::Local(h) => HybridSession::Local(ObjectHandle::begin(h, id).await?),
            Self::Remote(h) => HybridSession::Remote(ObjectHandle::begin(h, id).await?),
        })
    }
    async fn heartbeat(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        replay: Option<&Replay>,
    ) -> Result<Option<i64>, StoreError> {
        hybrid!(self.heartbeat(id, step_run_id, claim, owner, replay))
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
        hybrid!(self.append_logs(id, step_run_id, claim, owner, lines, replay))
    }
    async fn claim_record(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<Option<ClaimWrite>, StoreError> {
        hybrid!(self.claim_record(id, step_run_id))
    }
    async fn read_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        hybrid!(self.read_logs(id, step_run_id, after, limit))
    }
    async fn credential_family(
        &self,
        family_id: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        hybrid!(self.credential_family(family_id))
    }
    async fn credential_by_hash(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        hybrid!(self.credential_by_hash(hash))
    }
    async fn credential_by_id(
        &self,
        token_id: &TokenId,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        hybrid!(self.credential_by_id(token_id))
    }
    async fn family_by_idempotency_key(
        &self,
        id: &ExecutionId,
        key: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        hybrid!(self.family_by_idempotency_key(id, key))
    }
    async fn claim_due(&self, limit: usize, lease_ms: u64) -> Result<Vec<WorkLease>, StoreError> {
        hybrid!(self.claim_due(limit, lease_ms))
    }
    async fn ack(&self, id: &WorkId, generation: u64) -> Result<(), StoreError> {
        hybrid!(self.ack(id, generation))
    }
    async fn claim_settle_work(
        &self,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<WorkLease>, StoreError> {
        hybrid!(self.claim_settle_work(limit, lease_ms))
    }
    async fn current_execution(&self) -> Result<Option<ExecutionId>, StoreError> {
        hybrid!(self.current_execution())
    }
    async fn pending_derived(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<(u64, DerivedUpdate)>, StoreError> {
        hybrid!(self.pending_derived(after, limit))
    }
    async fn derived_row(&self, sequence: u64) -> Result<Option<DerivedUpdate>, StoreError> {
        hybrid!(self.derived_row(sequence))
    }
    async fn ack_derived(&self, through: u64) -> Result<u64, StoreError> {
        hybrid!(self.ack_derived(through))
    }
    async fn defer_outbox(&self, stream: OutboxStream) -> Result<i64, StoreError> {
        hybrid!(self.defer_outbox(stream))
    }
    async fn initialize_start(&self, init: StartInit) -> Result<(), StoreError> {
        hybrid!(self.initialize_start(init))
    }
    async fn admit(&self, id: &ExecutionId, admitted_at_ms: i64) -> Result<(), StoreError> {
        hybrid!(self.admit(id, admitted_at_ms))
    }
    async fn start_record(&self, id: &ExecutionId) -> Result<Option<StartRecord>, StoreError> {
        hybrid!(self.start_record(id))
    }
    async fn export_parts(
        &self,
        id: &ExecutionId,
    ) -> Result<(Vec<IdempotencyRecord>, Vec<WorkState>), StoreError> {
        hybrid!(self.export_parts(id))
    }
    async fn export_run(
        &self,
        id: &ExecutionId,
    ) -> Result<runbook_store_do::portable::RunExport, StoreError> {
        hybrid!(self.export_run(id))
    }
    async fn purge(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        hybrid!(self.purge(id))
    }
    async fn install(
        &self,
        id: &ExecutionId,
        unit: runbook_store_do::portable::RunInstall,
    ) -> Result<(), StoreError> {
        hybrid!(self.install(id, unit))
    }
    async fn release(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        hybrid!(self.release(id))
    }
    async fn log_flush(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<Option<PendingChunk>, StoreError> {
        hybrid!(self.log_flush(id, step_run_id))
    }
    async fn log_flushed(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        chunk: &LogChunk,
    ) -> Result<bool, StoreError> {
        hybrid!(self.log_flushed(id, step_run_id, chunk))
    }
    async fn log_chunks(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
    ) -> Result<Vec<LogChunk>, StoreError> {
        hybrid!(self.log_chunks(id, step_run_id, after))
    }
    async fn create_execution(
        &self,
        projection: runbook_core::ExecutionProjection,
    ) -> Result<(), StoreError> {
        hybrid!(self.create_execution(projection))
    }
}

/// Whether an op writes (the serving object then fans its new frames out
/// and publishes its notifications; its alarm settles the rest).
pub fn writes(op: &Op) -> bool {
    !matches!(
        op,
        Op::Load
            | Op::Begin
            | Op::ReplayAt { .. }
            | Op::ClaimRecord { .. }
            | Op::ReadLogs { .. }
            | Op::CredentialFamily { .. }
            | Op::CredentialByHash { .. }
            | Op::CredentialById { .. }
            | Op::FamilyByIdempotencyKey { .. }
            | Op::CurrentExecution
            | Op::PendingDerived { .. }
            | Op::DerivedRow { .. }
            | Op::StartRecord
            | Op::ExportParts
            | Op::ExportRun
            | Op::LogFlush { .. }
            | Op::LogChunks { .. }
            | Op::Now
            | Op::TableCounts
    )
}
