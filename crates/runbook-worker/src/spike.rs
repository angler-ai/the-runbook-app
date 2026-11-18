//! CF1: the portable half of the spike. Everything here is plain
//! `runbook-core`/`runbook-engine` code with no Workers types, so it runs
//! natively under `cargo test` and inside the ExecutionDO unchanged.
//!
//! The spike runbook waits one second and then readies a manual step, so a
//! start commit carries a timer (the DO alarm) and the alarm's commit
//! advances the run (`pause` succeeds, `next` becomes ready).
use runbook_core::{
    CommitResult, ExecutionCommit, ExecutionStore, StoreError, StoredExecution,
    catalog::{DerivedUpdate, RunSummary, TenantScope},
    directory::{DirectorySnapshot, OrganizationRecord},
    runtime::Runtime,
    start::project_preserving,
};
use runbook_engine::{ExecutionState, StartContext, apply, prepare_start};
use runbook_model::{ActorRef, ExecutionId, ExecutionStatus, OrganizationId, ProjectId};
use runbook_spec::CompiledRunbook;
use std::rc::Rc;

/// The compiled-in spike runbook: a 1 s duration wait, then a manual step.
pub const SPIKE_RUNBOOK: &str = "apiVersion: therunbook.app/v1
kind: Runbook
metadata: {id: cf1-spike, title: CF1 spike}
spec:
  steps:
    - {id: pause, wait: {for: 1s}}
    - {id: next, title: Next}
";

/// Fixed spike identities. The spike Worker has no credentials, so every
/// object is this tenant's; real routes take the scope only from verified
/// credentials (§18, CF3).
pub const ORG_ID: &str = "org_00000000000000000000000CF1";
pub const PROJECT_ID: &str = "prj_00000000000000000000000CF1";
pub const RUNBOOK_ID: &str = "rb_00000000000000000000000CF1";
pub const VERSION_ID: &str = "rbv_00000000000000000000000CF1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpikeError {
    Compile(String),
    Engine(String),
    Store(StoreError),
}
impl SpikeError {
    pub fn code(&self) -> String {
        match self {
            Self::Compile(code) | Self::Engine(code) => code.clone(),
            Self::Store(StoreError::NotFound) => "store.not_found".into(),
            Self::Store(StoreError::InvalidBatch(code) | StoreError::Backend(code)) => {
                (*code).into()
            }
        }
    }
}
impl From<StoreError> for SpikeError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<runbook_core::actor::ActorError> for SpikeError {
    fn from(error: runbook_core::actor::ActorError) -> Self {
        Self::Engine(error.code().into())
    }
}
impl From<runbook_engine::EngineError> for SpikeError {
    fn from(error: runbook_engine::EngineError) -> Self {
        Self::Engine(error.code().into())
    }
}

fn id<T: std::str::FromStr>(text: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    text.parse().expect("fixed CF1 identity")
}

pub fn scope() -> TenantScope {
    TenantScope {
        org_id: id::<OrganizationId>(ORG_ID),
        project_id: id::<ProjectId>(PROJECT_ID),
    }
}

/// Compile the spike runbook with the shared compiler (`runbook-spec`).
pub fn compiled() -> Result<CompiledRunbook, SpikeError> {
    let spec = runbook_spec::parse(SPIKE_RUNBOOK)
        .map_err(|d| SpikeError::Compile(first_code(d.iter().map(|d| d.code.as_str()))))?;
    runbook_spec::compile(&spec, &Default::default())
        .map_err(|d| SpikeError::Compile(first_code(d.iter().map(|d| d.code.as_str()))))
}

fn first_code<'a>(mut codes: impl Iterator<Item = &'a str>) -> String {
    codes.next().unwrap_or("spec.invalid").to_owned()
}

fn empty_directory() -> DirectorySnapshot {
    let org_id = id::<OrganizationId>(ORG_ID);
    DirectorySnapshot {
        org_id: org_id.clone(),
        revision: 0,
        organization: OrganizationRecord {
            id: org_id,
            title: "CF1".into(),
        },
        users: Default::default(),
        projects: Default::default(),
        teams: Default::default(),
        project_members: Default::default(),
        team_members: Default::default(),
    }
}

fn runtime(state: ExecutionState, at_ms: i64, after_sequence: u64) -> Runtime {
    let execution_id = state.id.clone();
    Runtime::new(
        state,
        empty_directory(),
        scope(),
        at_ms,
        runbook_core::step_lookup::ExecutionStepIds::new(&execution_id, after_sequence),
    )
}

fn is_terminal(status: ExecutionStatus) -> bool {
    matches!(
        status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
}

/// Seal the runtime's pending work into one plain CAS commit: events,
/// projection, timer diff, notification outbox and the derived D1 row.
fn seal(runtime: &Runtime, previous: &StoredExecution) -> ExecutionCommit {
    let state = &runtime.state;
    let projection = project_preserving(
        state,
        id(RUNBOOK_ID),
        id(VERSION_ID),
        previous.projection.completed_at_ms,
    );
    let summary = RunSummary {
        execution_id: state.id.clone(),
        runbook_id: projection.runbook_id.clone(),
        sequence: projection.sequence,
        status: projection.status,
        completed_at_ms: projection.completed_at_ms,
        title: state.title.clone(),
        tags: state.tags.clone(),
        fields: Default::default(),
        created_by: state.created_by.clone(),
        created_at_ms: 0,
    };
    ExecutionCommit {
        execution_id: state.id.clone(),
        expected_sequence: previous.revision,
        events: runtime.pending_events().to_vec(),
        timers: runtime.timer_changes(previous),
        outbox: runtime.intents().to_vec(),
        idempotency: vec![],
        claims: vec![],
        credentials: vec![],
        derived_outbox: vec![DerivedUpdate {
            assignments: vec![],
            release_concurrency: is_terminal(projection.status),
            summary,
            memory: vec![],
        }],
        projection,
    }
}

/// The empty head a start commit diffs against (revision 0, no rows).
fn unstarted(state: &ExecutionState) -> StoredExecution {
    StoredExecution {
        revision: 0,
        projection: project_preserving(state, id(RUNBOOK_ID), id(VERSION_ID), None),
        events: vec![],
        timers: Default::default(),
        timer_history: Default::default(),
        outbox: Default::default(),
        idempotency: Default::default(),
        claims: Default::default(),
        derived_outbox: Default::default(),
    }
}

/// Start the spike runbook at the store's clock and settle it to its first
/// ready work: `ExecutionStarted`, the wait step and its timer.
pub fn start_commit(execution_id: &ExecutionId, at_ms: i64) -> Result<ExecutionCommit, SpikeError> {
    let state = ExecutionState::new(execution_id.clone(), Rc::new(compiled()?));
    let previous = unstarted(&state);
    let event = prepare_start(&state, &StartContext::default(), at_ms)?;
    let mut runtime = runtime(state, at_ms, 0);
    runtime.append(event, ActorRef::System)?;
    runtime.settle()?;
    Ok(seal(&runtime, &previous))
}

/// Rebuild reducer state from the stored event log (the authoritative
/// state; the projection is only derived).
pub fn replay(stored: &StoredExecution) -> Result<ExecutionState, SpikeError> {
    let id = stored.projection.execution_id.clone();
    let mut state = ExecutionState::new(id, Rc::new(compiled()?));
    for envelope in &stored.events {
        state = apply(&state, envelope)?;
    }
    Ok(state)
}

/// Feed due timers and time-driven transitions at `at_ms` (the alarm's
/// store clock), as the native drain worker does. `None` when nothing was
/// due yet.
pub fn advance_commit(
    stored: &StoredExecution,
    at_ms: i64,
) -> Result<Option<ExecutionCommit>, SpikeError> {
    let state = replay(stored)?;
    let mut runtime = runtime(state, at_ms, stored.revision);
    for _ in 0..1000 {
        let before = runtime.pending_events().len();
        for event in runbook_engine::due_timers(&runtime.state, at_ms)? {
            runtime.append(event, ActorRef::System)?;
        }
        for command in runbook_engine::advance_time(&runtime.state, at_ms)? {
            runtime.realize(command)?;
        }
        runtime.settle()?;
        if runtime.pending_events().len() == before {
            break;
        }
    }
    Ok(runtime.has_pending().then(|| seal(&runtime, stored)))
}

/// Drive one start through any `ExecutionStore` (the in-memory fake
/// natively, the DO SQLite store on Workers).
pub async fn start_with<S: ExecutionStore>(
    store: &S,
    execution_id: &ExecutionId,
    at_ms: i64,
) -> Result<CommitResult, SpikeError> {
    Ok(store.commit(start_commit(execution_id, at_ms)?).await?)
}

/// The event type tag of a stored envelope, for state dumps.
pub fn event_type(envelope: &runbook_engine::EventEnvelope) -> String {
    serde_json::to_value(&envelope.event)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned))
        .unwrap_or_default()
}

/// How long the alarm handler holds the work it claims.
pub const ALARM_LEASE_MS: u64 = 60_000;
/// How much due work one alarm takes.
pub const ALARM_BATCH: usize = 16;

fn code(error: &StoreError) -> String {
    SpikeError::Store(error.clone()).code()
}

/// One alarm over this object's store: lease the due timers (only those:
/// other work stays free for its own consumer, CF3's drain and CF4's
/// delivery) and advance the run at the store clock, as the native drain
/// does. A timer's lease is acknowledged only once its advance committed,
/// or when the queue says it is already obsolete; one the advance does not
/// fire keeps its lease, which expires and offers it again. An outbox
/// stream whose retry is due has no consumer here yet, so it is deferred
/// with backoff. Portable, so it is tested natively; `edge` calls it.
pub async fn run_alarm<B: runbook_store_do::DoSql>(
    store: &runbook_store_do::DoStore<B>,
) -> Result<String, String> {
    use runbook_core::work_queue::{WorkKind, WorkQueue};
    use runbook_store_do::OutboxStream;
    let id = store.name().clone();
    let leases = store
        .claim_due_of(ALARM_BATCH, ALARM_LEASE_MS, |kind| {
            matches!(kind, WorkKind::Timer { .. })
        })
        .map_err(|e| code(&e))?;
    let mut outcome = vec![];
    for lease in &leases {
        let run = store.load(&id).await.map_err(|e| code(&e))?;
        let fired = match advance_commit(&run, store.now_ms()).map_err(|e| e.code())? {
            None => {
                outcome.push("not_due".to_owned());
                false
            }
            Some(commit) => match store.commit(commit).await.map_err(|e| code(&e))? {
                CommitResult::Committed(sequence) => {
                    outcome.push(format!("advanced:{sequence}"));
                    true
                }
                CommitResult::Conflict => return Err("store.conflict".to_owned()),
            },
        };
        if !fired {
            continue;
        }
        // The fired timer's row is gone, so the queue usually reports the
        // lease obsolete: that is done too.
        match store.ack(&lease.id, lease.generation).await {
            Ok(()) | Err(StoreError::InvalidBatch("queue.work_obsolete")) => {}
            Err(error) => return Err(code(&error)),
        }
    }
    let now = store.now_ms();
    for stream in [OutboxStream::Derived, OutboxStream::Notification] {
        if store
            .outbox_retry_at(stream)
            .map_err(|e| code(&e))?
            .is_some_and(|at| at <= now)
        {
            store.defer_outbox(stream).map_err(|e| code(&e))?;
            outcome.push(format!("deferred:{}", stream.as_str()));
        }
    }
    if outcome.is_empty() {
        outcome.push("not_due".to_owned());
    }
    Ok(outcome.join(","))
}
