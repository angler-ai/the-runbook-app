//! Start admission (B5a): one plain start commit carries execution
//! initialization, the admission request and the replay result, so the
//! adapter persists all or none. Queued runs materialize no actor steps
//! and spend no deadline; activation (D24) applies the stored start
//! context with admitted_at as the start time.
use crate::{
    ExecutionCommit, ExecutionProjection, ExecutionStore, StepProjection, StoreError,
    blob::{BlobKey, BlobStore},
    catalog::{Admission, AdmitRequest, CatalogStore, DerivedUpdate, RunSummary},
    search::project_search_fields,
    work_queue::{WorkKind, WorkLease, WorkQueue},
};
use runbook_engine::{
    EventEnvelope, ExecutionLimits, ExecutionState, StartContext, apply, prepare_start,
};
use runbook_model::{
    ActorRef, Data, Diagnostic, ExecutionId, FieldError, RunbookId, VersionId, VersionSelector,
};
use serde::{Deserialize, Serialize};
use std::{future::Future, num::NonZeroU32, rc::Rc};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartOutcome {
    pub execution_id: ExecutionId,
    pub runbook_version_id: VersionId,
    /// Null for preview versions; identity is the rbv_ ID.
    pub runbook_version: Option<NonZeroU32>,
    pub admission: Option<Admission>,
}

/// Pinned start facts recorded when the start is accepted. Activation
/// replays `context` verbatim; publishing newer versions changes nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StartRecord {
    pub runbook_id: RunbookId,
    pub version_id: VersionId,
    pub version: Option<NonZeroU32>,
    pub semantic_hash: [u8; 32],
    pub snapshot_digest: [u8; 32],
    pub context: StartContext,
    pub concurrency_key: Option<String>,
    /// B9: the run this one reruns; recorded once at start, never changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun_of: Option<ExecutionId>,
    /// The tags the caller asked for, without the version's spec tags; a
    /// rerun on another version recombines them with that version's (R28).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartReplay {
    pub key: String,
    pub principal: String,
    pub request_hash: [u8; 32],
    pub outcome: StartOutcome,
}

/// Backend-neutral start write. `summary.created_at_ms` is overwritten by
/// the adapter with its store clock inside the transaction.
#[derive(Clone, Debug, PartialEq)]
pub struct StartCommit {
    pub projection: ExecutionProjection,
    pub record: StartRecord,
    pub admission: Option<AdmitRequest>,
    /// LANG3: the version's `on_conflict` policy (`None` is `queue`). The
    /// adapter resolves it with `concurrency::resolve` under its pool lock.
    pub on_conflict: Option<crate::concurrency::OnConflict>,
    pub derived: DerivedUpdate,
    pub replay: Option<StartReplay>,
}

/// CF12b review round 1: the most bytes a start's record, its start event
/// or its summary may take as JSON: the 2,000,000 bytes a D1 or Durable
/// Object row holds, less 64 KiB for the rest of the row. The API caps the
/// data a request carries; this also counts what core merges in (the
/// version's input and field defaults, its spec tags and variables, a
/// rendered title), and a start past it is refused with
/// `request.value_too_large`, writing nothing.
pub const MAX_START_ROW_BYTES: usize = 2_000_000 - 65_536;

/// D20: start replay results are retained for 24 hours.
pub const REPLAY_RETENTION_MS: i64 = 24 * 3_600_000;

pub trait StartStore: ExecutionStore {
    /// Atomically: replay check, owner claim, head, start record,
    /// pooled admission (D25), derived summary, replay save.
    fn commit_start(
        &self,
        commit: StartCommit,
    ) -> impl Future<Output = Result<StartOutcome, StoreError>>;
    fn start_record(
        &self,
        id: &ExecutionId,
    ) -> impl Future<Output = Result<Option<StartRecord>, StoreError>>;
    /// Tenant-scoped saved start result for a replay key, callable before
    /// any preparation. Rows older than the retention window are purged
    /// opportunistically and read as absent (D20).
    fn find_replay(
        &self,
        idempotency_key: &str,
    ) -> impl Future<Output = Result<Option<StartReplay>, StoreError>>;
    /// Remove one queued waiter. False when it was not queued. The
    /// execution's terminal bookkeeping belongs to B6 cancellation.
    fn cancel_queued(
        &self,
        id: &ExecutionId,
        replay: Option<&crate::actor::Replay>,
    ) -> impl Future<Output = Result<bool, StoreError>>;
}

#[derive(Clone, Debug)]
pub struct StartCommand {
    pub execution_id: ExecutionId,
    pub runbook_id: RunbookId,
    pub version: Option<VersionSelector>,
    pub inputs: Data,
    pub fields: Data,
    pub project_vars: Data,
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub url: String,
    pub created_by: String,
    pub limits: ExecutionLimits,
    pub idempotency_key: String,
    pub principal: String,
    pub request_hash: [u8; 32],
}

#[derive(Debug)]
pub enum StartError {
    Store(StoreError),
    Diagnostics(Vec<Diagnostic>),
    /// The caller's inputs or fields do not satisfy the version's schemas:
    /// one error per problem, paths under `inputs.`/`fields.` (U10). The
    /// same code and shape as a rejected rerun.
    InvalidData(Vec<FieldError>),
    VersionNotFound,
    /// LANG3 `cancel_previous`: more than `concurrency::MAX_PREEMPTED_RUNS`
    /// runs would have to be cancelled.
    ConcurrencyTooManyHolders,
    /// LANG3 `cancel_previous`: this run, which holds or waits for the key,
    /// could not be prepared for cancellation.
    ConcurrencyHolderUnavailable(ExecutionId),
    /// LANG3 `cancel_previous`: the pool kept changing under the start.
    ConcurrencyContended,
}
impl StartError {
    pub const INVALID_DATA_CODE: &'static str = crate::rerun::RerunRejected::CODE;
}

/// Commit a prepared start under its version's `on_conflict` policy
/// (LANG3), through the one start transaction (B5a, D24/D25).
///
/// A saved start for the same idempotency key replays first, preparing and
/// cancelling nothing. `queue` and `reject` commit once: a refused start is
/// `StoreError::InvalidBatch(concurrency::CONFLICT_CODE)` and writes
/// nothing. For `cancel_previous` the core reads the key's pool and, when
/// the start would wait, prepares a cancellation for each of
/// `concurrency::evictions` (at most `MAX_PREEMPTED_RUNS`); the adapter
/// applies them only if its locked pool still matches, otherwise this
/// prepares again (at most `MAX_PREEMPTION_ATTEMPTS` times, then
/// `ConcurrencyContended`).
pub async fn commit_start_with_policy<S, D>(
    store: &S,
    directory: &D,
    scope: &crate::catalog::TenantScope,
    mut commit: StartCommit,
) -> Result<StartOutcome, StartError>
where
    S: StartStore + CatalogStore + BlobStore + crate::session::SessionStore,
    D: crate::directory::DirectoryStore,
{
    use crate::concurrency::{
        OnConflict, PreparationError, STALE_CODE, cancellation_commits, evictions, would_queue,
    };
    if let Some(replay) = &commit.replay
        && let Some(saved) = store.find_replay(&replay.key).await?
    {
        if saved.principal != replay.principal || saved.request_hash != replay.request_hash {
            return Err(StartError::Store(StoreError::InvalidBatch(
                "request.idempotency_key_reused",
            )));
        }
        return Ok(saved.outcome);
    }
    let superseded = runbook_engine::Supersession {
        execution_id: commit.projection.execution_id.clone(),
        started_by: commit.record.context.created_by.clone(),
    };
    for _ in 0..crate::concurrency::MAX_PREEMPTION_ATTEMPTS {
        if let (Some(OnConflict::CancelPrevious { view, commits }), Some(request)) =
            (&mut commit.on_conflict, &commit.admission)
        {
            let current = store.admission_pool(&request.key).await?;
            let evicted = if would_queue(request.limit, &current) {
                evictions(request.limit, &current)
            } else {
                vec![]
            };
            *commits =
                match cancellation_commits(store, directory, scope, &evicted, &superseded).await {
                    Ok(prepared) => prepared,
                    Err(PreparationError::TooMany) => {
                        return Err(StartError::ConcurrencyTooManyHolders);
                    }
                    Err(PreparationError::Unavailable(id)) => {
                        return Err(StartError::ConcurrencyHolderUnavailable(id));
                    }
                    Err(PreparationError::Store(error)) => return Err(StartError::Store(error)),
                };
            *view = current;
        }
        match store.commit_start(commit.clone()).await {
            Err(StoreError::InvalidBatch(STALE_CODE)) => continue,
            other => return other.map_err(StartError::Store),
        }
    }
    Err(StartError::ConcurrencyContended)
}

impl From<StoreError> for StartError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(StoreError::InvalidBatch(code)) => f.write_str(code),
            Self::Store(_) => f.write_str("start.store_failed"),
            Self::Diagnostics(_) => f.write_str("start.invalid"),
            Self::InvalidData(_) => f.write_str(Self::INVALID_DATA_CODE),
            Self::VersionNotFound => f.write_str("runbook.version_not_found"),
            Self::ConcurrencyTooManyHolders => {
                f.write_str(crate::concurrency::TOO_MANY_HOLDERS_CODE)
            }
            Self::ConcurrencyHolderUnavailable(_) => {
                f.write_str(crate::concurrency::HOLDER_UNAVAILABLE_CODE)
            }
            Self::ConcurrencyContended => f.write_str(crate::concurrency::CONTENDED_CODE),
        }
    }
}
impl std::error::Error for StartError {}

/// Project engine state into the stored derived projection. Only
/// materialized step runs are projected; paths are step IDs at this
/// layer (the document view owns full paths).
pub fn project(
    state: &ExecutionState,
    runbook_id: RunbookId,
    version_id: VersionId,
) -> ExecutionProjection {
    project_preserving(state, runbook_id, version_id, None)
}

/// Project state, keeping a previously stored completion time: terminal
/// facts stay frozen across later commits (amendments included, B9a).
pub fn project_preserving(
    state: &ExecutionState,
    runbook_id: RunbookId,
    version_id: VersionId,
    previous_completed_at_ms: Option<i64>,
) -> ExecutionProjection {
    ExecutionProjection {
        execution_id: state.id.clone(),
        runbook_id,
        version_id,
        compiled_hash: *state.compiled().hash(),
        engine_version: state.engine_version,
        sequence: state.sequence,
        // The reported status: `waiting` when only people or timers can
        // move the run (R50).
        status: runbook_engine::reported_status(state),
        phase: state.phase,
        completed_at_ms: previous_completed_at_ms.or_else(|| {
            matches!(
                state.status,
                runbook_model::ExecutionStatus::Succeeded
                    | runbook_model::ExecutionStatus::Failed
                    | runbook_model::ExecutionStatus::Cancelled
            )
            .then_some(state.at_ms)
            .filter(|_| state.sequence > 0)
        }),
        result: serde_json::Value::Null,
        outputs: Data::new(),
        steps: state
            .steps
            .iter()
            .filter_map(|step| {
                step.id.as_ref().map(|id| StepProjection {
                    execution_id: state.id.clone(),
                    step_run_id: id.clone(),
                    path: step.step_id.clone(),
                    status: step.status,
                    verdict: step.verdict,
                    outputs: step.outputs.clone(),
                    summary: step.summary.clone(),
                    runner: runner_projection(state, step),
                })
            })
            .collect(),
    }
}

/// R0-4: a runner agent step's agent and limit, for the ready listing.
fn runner_projection(
    state: &ExecutionState,
    step: &runbook_engine::StepState,
) -> Option<crate::StepRunnerProjection> {
    use runbook_spec::ir::{AgentTarget, NodeKind};
    let node = state.node(&step.step_id)?;
    if let NodeKind::Approval(approval) = &node.kind {
        let AgentTarget::Runner { agent_id, .. } = approval.agent_target.as_ref()? else {
            return None;
        };
        let record = step.approval.as_ref()?;
        let limits = approval.agent_limits.as_ref();
        let decision = (runbook_engine::decision_open(state, step)
            && state.pause_reasons.is_empty()
            && record.decision_lease.is_none()
            && record.agent_notes.is_empty())
        .then(|| crate::decisions::decision_input(state, step, approval));
        return Some(crate::StepRunnerProjection {
            agent_id: agent_id.clone(),
            ready_at_ms: record.requested.as_ref()?.request.agent_ready_at_ms?,
            max_cost_usd: limits.and_then(|l| l.max_cost_usd),
            decision,
        });
    }
    let NodeKind::Task(task) = &node.kind else {
        return None;
    };
    // After a handoff the task continues with a person only (claims refuse
    // the runner), so it is no longer runner work.
    if step.human_continuation {
        return None;
    }
    let runbook_spec::ir::AgentTarget::Runner { agent_id, .. } = task.agent_target.as_ref()? else {
        return None;
    };
    Some(crate::StepRunnerProjection {
        agent_id: agent_id.clone(),
        ready_at_ms: step.opened_at_ms.unwrap_or(state.at_ms),
        max_cost_usd: task.agent_limits.as_ref().and_then(|l| l.max_cost_usd),
        decision: None,
    })
}

pub(crate) fn summary_of(state: &ExecutionState, projection: &ExecutionProjection) -> RunSummary {
    let fields = project_search_fields(
        &state.compiled().ir().fields,
        &state.fields,
        crate::search::DEFAULT_MAX_INDEXED_FIELDS,
    )
    .unwrap_or_default();
    RunSummary {
        execution_id: projection.execution_id.clone(),
        runbook_id: projection.runbook_id.clone(),
        sequence: projection.sequence,
        status: projection.status,
        completed_at_ms: projection.completed_at_ms,
        title: state.title.clone(),
        tags: state.tags.clone(),
        fields,
        created_by: state.created_by.clone(),
        created_at_ms: 0, // adapter stamps the store clock
    }
}

/// Resolve the version once, restore its snapshot, validate and default
/// inputs/fields, evaluate the concurrency key once, and build the plain
/// start commit. Nothing is written here.
pub async fn prepare_start_commit(
    catalog: &impl CatalogStore,
    blobs: &impl BlobStore,
    command: &StartCommand,
) -> Result<StartCommit, StartError> {
    if command.idempotency_key.is_empty() {
        return Err(StartError::Store(StoreError::InvalidBatch(
            "request.idempotency_key_required",
        )));
    }
    // Fast replay path needs no compilation; commit_start rechecks it
    // atomically, so a racing duplicate still returns the saved outcome.
    let resolved = catalog
        .resolve_version(&command.runbook_id, command.version.clone())
        .await
        .map_err(|error| match error {
            StoreError::NotFound => StartError::VersionNotFound,
            other => StartError::Store(other),
        })?;
    let record = version_record(catalog, &command.runbook_id, &resolved.runbook_version_id).await?;
    let bytes = blobs
        .get(&BlobKey::Shared {
            hash: record.snapshot_digest,
        })
        .await?;
    let diagnostics = |diagnostics: Vec<runbook_spec::Diagnostic>| {
        StartError::Diagnostics(
            diagnostics
                .into_iter()
                .map(|d| Diagnostic {
                    path: d.path,
                    message: d.message.unwrap_or_else(|| d.code.clone()),
                    code: d.code,
                    line: u32::try_from(d.line).ok().filter(|n| *n > 0),
                    column: u32::try_from(d.column).ok().filter(|n| *n > 0),
                })
                .collect(),
        )
    };
    let compiled = runbook_spec::restore(&bytes).map_err(diagnostics)?;
    // CF13b: a version published before a publish-time limit restores (its
    // runs load), but a new run on it is refused (a call's child: the step
    // fails with `start.invalid`).
    compiled.admit_new_run().map_err(diagnostics)?;
    if compiled.hash() != &record.semantic_hash {
        return Err(StartError::Store(StoreError::Backend(
            "start.snapshot_hash_mismatch",
        )));
    }
    // U10: report every invalid input and field against the version's
    // schemas before the engine sees them (it stops at the first).
    let mut errors = vec![];
    for (root, schema, data) in [
        ("inputs", &compiled.ir().inputs, &command.inputs),
        ("fields", &compiled.ir().fields, &command.fields),
    ] {
        crate::rerun::check_data(root, schema, data, &mut errors, start_message);
    }
    if !errors.is_empty() {
        return Err(StartError::InvalidData(errors));
    }
    let state = ExecutionState::new(command.execution_id.clone(), Rc::new(compiled));
    let context = StartContext {
        limits: command.limits,
        inputs: command.inputs.clone(),
        fields: command.fields.clone(),
        project_vars: command.project_vars.clone(),
        title: command.title.clone(),
        tags: command.tags.clone(),
        url: command.url.clone(),
        created_by: command.created_by.clone(),
        parent: None,
    };
    // Validate and default now (the same prepare_start activation will
    // replay); the working state also renders title and concurrency key.
    let working = working_state(&state, &context)?;
    let resolved_context = StartContext {
        limits: command.limits,
        inputs: working.inputs.clone(),
        fields: working.fields.clone(),
        project_vars: working.project_vars.clone(),
        title: Some(match runbook_engine::evaluate_execution_title(&working) {
            Ok(Some(title)) => title,
            _ => working.title.clone(),
        }),
        tags: {
            let mut tags = state.compiled().ir().execution.tags.clone();
            for tag in &command.tags {
                if !tags.contains(tag) {
                    tags.push(tag.clone());
                }
            }
            tags
        },
        url: command.url.clone(),
        created_by: command.created_by.clone(),
        parent: None,
    };
    let concurrency = crate::concurrency::resolve_concurrency(&working)
        .map_err(|_| StartError::Store(StoreError::InvalidBatch("start.invalid")))?;
    let on_conflict = state
        .compiled()
        .ir()
        .execution
        .concurrency
        .as_ref()
        .and_then(|c| crate::concurrency::OnConflict::for_policy(c.on_conflict));
    let projection = project(
        &state,
        command.runbook_id.clone(),
        resolved.runbook_version_id.clone(),
    );
    let admission = concurrency.as_ref().map(|(key, limit)| AdmitRequest {
        execution_id: command.execution_id.clone(),
        key: key.clone(),
        limit: *limit,
        idempotency_key: command.idempotency_key.clone(),
    });
    let outcome = StartOutcome {
        execution_id: command.execution_id.clone(),
        runbook_version_id: resolved.runbook_version_id.clone(),
        runbook_version: resolved.runbook_version,
        admission: None, // filled by commit_start from the pooled result
    };
    let commit = StartCommit {
        record: StartRecord {
            runbook_id: command.runbook_id.clone(),
            version_id: resolved.runbook_version_id.clone(),
            version: resolved.runbook_version,
            semantic_hash: record.semantic_hash,
            snapshot_digest: record.snapshot_digest,
            context: resolved_context,
            concurrency_key: concurrency.map(|(key, _)| key),
            rerun_of: None,
            request_tags: command.tags.clone(),
        },
        derived: DerivedUpdate {
            summary: summary_of(&working, &projection),
            assignments: vec![],
            release_concurrency: false,
            memory: vec![],
        },
        projection,
        admission,
        on_conflict,
        replay: Some(StartReplay {
            key: command.idempotency_key.clone(),
            principal: command.principal.clone(),
            request_hash: command.request_hash,
            outcome,
        }),
    };
    // CF12b review round 1: the record, the start event activation will
    // write (the same context) and the summary, each in one row.
    let started = prepare_start(&state, &commit.record.context, 0)
        .map_err(|_| StartError::Store(StoreError::InvalidBatch("start.invalid")))?;
    let fits = |bytes: Result<Vec<u8>, serde_json::Error>| {
        bytes.is_ok_and(|b| b.len() <= MAX_START_ROW_BYTES)
    };
    if !fits(serde_json::to_vec(&commit.record))
        || !fits(serde_json::to_vec(&started))
        || !fits(serde_json::to_vec(&commit.derived.summary))
    {
        return Err(StartError::Store(StoreError::InvalidBatch(VALUE_TOO_LARGE)));
    }
    Ok(commit)
}

/// The code of a start refused by [`MAX_START_ROW_BYTES`] (422), the
/// API's code for a request value past its cap.
pub const VALUE_TOO_LARGE: &str = "request.value_too_large";

/// A person-readable message for one start data error, by its `data.*` code.
fn start_message(code: &str) -> String {
    match code {
        "data.required" => "This value is required.",
        "data.type_error" => "This value has the wrong type.",
        "data.enum" => "This value is not one of the allowed options.",
        "data.range" => "This value is outside the allowed range.",
        "data.unknown_field" => "This version does not declare this value.",
        "data.depth_limit" => "This value is nested too deeply.",
        _ => "This value does not satisfy this version.",
    }
    .into()
}

fn working_state(
    state: &ExecutionState,
    context: &StartContext,
) -> Result<ExecutionState, StartError> {
    let event = prepare_start(state, context, 0)
        .map_err(|_| StartError::Store(StoreError::InvalidBatch("start.invalid")))?;
    let envelope = EventEnvelope {
        execution_id: state.id.clone(),
        seq: 1,
        at_ms: 0,
        actor: ActorRef::System,
        event,
    };
    apply(state, &envelope)
        .map_err(|_| StartError::Store(StoreError::InvalidBatch("start.invalid")))
}

async fn version_record(
    catalog: &impl CatalogStore,
    runbook_id: &RunbookId,
    version_id: &VersionId,
) -> Result<crate::catalog::VersionRecord, StartError> {
    // Resolve already proved the version; fetch its row through list APIs.
    for view in catalog.list_versions(runbook_id).await? {
        let _ = view;
    }
    catalog
        .version_record(runbook_id, version_id)
        .await?
        .ok_or(StartError::VersionNotFound)
}

/// D24 activation: apply the stored start context at admitted_at. Plain
/// CAS commit (not the session fence, whose transaction time would move
/// the deadline anchor): a losing worker sees Conflict and just acks.
pub async fn activate<S>(
    store: &S,
    directory: &impl crate::directory::DirectoryStore,
    scope: crate::catalog::TenantScope,
    lease: &WorkLease,
) -> Result<(), StoreError>
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + crate::session::SessionStore,
{
    let id = &lease.id.execution_id;
    let current = store.load(id).await?;
    if current.revision > 0 {
        return store.ack(&lease.id, lease.generation).await;
    }
    let record = store.start_record(id).await?.ok_or(StoreError::NotFound)?;
    // Pooled runs anchor at admission; a start with no concurrency key
    // was never queued, so it anchors at its start transaction time.
    let admitted_at = match store.admission(id).await? {
        Some(admission) => admission
            .admitted_at_ms
            .ok_or(StoreError::Backend("start.missing_admitted_at"))?,
        None => {
            store
                .get_run(id)
                .await?
                .ok_or(StoreError::NotFound)?
                .created_at_ms
        }
    };
    let bytes = store
        .get(&BlobKey::Shared {
            hash: record.snapshot_digest,
        })
        .await?;
    let compiled = runbook_spec::restore(&bytes)
        .map_err(|_| StoreError::Backend("start.snapshot_restore_failed"))?;
    let state = ExecutionState::new(id.clone(), Rc::new(compiled));
    let mut event = prepare_start(&state, &record.context, admitted_at)
        .map_err(|_| StoreError::InvalidBatch("start.invalid"))?;
    // LANG4: a child whose parent no longer awaits it (cancelled while the
    // child was being started) is cancelled before it starts, so an orphan
    // never runs, whatever crashed between its start and its parent.
    if let Some(link) = &record.context.parent
        && !crate::calls::wanted(store, id, link).await?
        && let runbook_engine::Event::ExecutionStarted { data, .. } = event
    {
        event = runbook_engine::Event::ExecutionCancelledBeforeStart {
            data,
            superseded_by: None,
            started_by: None,
        };
    }
    let directory_snapshot = match directory.snapshot(&scope.org_id).await {
        Ok(snapshot) => snapshot,
        Err(crate::directory::DirectoryError::NotFound) => crate::directory::DirectorySnapshot {
            org_id: scope.org_id.clone(),
            revision: 0,
            security: Default::default(),
            organization: crate::directory::OrganizationRecord {
                id: scope.org_id.clone(),
                title: String::new(),
            },
            users: Default::default(),
            projects: Default::default(),
            teams: Default::default(),
            project_members: Default::default(),
            team_members: Default::default(),
            archived_projects: Default::default(),
        },
        Err(_) => return Err(StoreError::Backend("actor.directory_unavailable")),
    };
    // R13: settle to the first ready steps through the shared runtime and
    // persist its timers and intents in the same commit as ExecutionStarted.
    let mut runtime = crate::runtime::Runtime::new(
        state,
        directory_snapshot,
        scope,
        admitted_at,
        crate::step_lookup::ExecutionStepIds::new(id, 0),
    );
    runtime
        .append(event, ActorRef::System)
        .map_err(|_| StoreError::InvalidBatch("start.invalid"))?;
    // CF13b review round 1: activation is held to the run ceiling like
    // every later settle. Its log starts empty, and its settle may run many
    // `set` steps (up to 1,000 rounds) before the run first rests; past
    // the ceiling an engine version 6 run is faulted and closes failed in
    // this commit.
    runtime.hold_to_ceiling(0, crate::run_budget::run_ceiling(store.run_budget()));
    runtime
        .settle()
        .map_err(|_| StoreError::InvalidBatch("start.invalid"))?;
    let projection = project(
        &runtime.state,
        record.runbook_id.clone(),
        record.version_id.clone(),
    );
    let summary = summary_of(&runtime.state, &projection);
    let timers = runtime.timer_changes(&current);
    let result = store
        .commit(ExecutionCommit {
            execution_id: id.clone(),
            expected_sequence: 0,
            events: runtime.pending_events().to_vec(),
            projection,
            timers,
            outbox: runtime.intents().to_vec(),
            idempotency: vec![],
            claims: vec![],
            credentials: vec![],
            derived_outbox: vec![DerivedUpdate {
                assignments: crate::runtime::assignments(
                    &runtime.state,
                    &Default::default(),
                    runtime.state.sequence,
                    &[],
                ),
                summary,
                // A run can finish while it is activated (it may fail at
                // once, F16); its concurrency key is released with it.
                release_concurrency: matches!(
                    runtime.state.status,
                    runbook_model::ExecutionStatus::Succeeded
                        | runbook_model::ExecutionStatus::Failed
                        | runbook_model::ExecutionStatus::Cancelled
                ),
                memory: vec![],
            }],
        })
        .await;
    match result {
        Ok(_) => {}
        Err(error) => return Err(error),
    }
    store.ack(&lease.id, lease.generation).await
}

/// C1 `execution.queued`: the outbox intent an adapter writes in the start
/// transaction when admission queued the run behind its concurrency key
/// (D24/D25). Sequence 0: no execution event exists yet. The ID is stable
/// for the execution, so a fan-out retry keeps it.
pub fn queued_intent(
    execution_id: &ExecutionId,
    record: &StartRecord,
    at_ms: i64,
) -> runbook_engine::NotificationIntent {
    runbook_engine::NotificationIntent {
        event_id: crate::webhooks::derived_id(
            "evt_",
            at_ms,
            &[execution_id.as_str(), "execution.queued"],
        ),
        kind: "execution.queued".into(),
        execution_id: execution_id.clone(),
        sequence: 0,
        at_ms,
        step_run_ids: vec![],
        recipients: vec![],
        reason: None,
        link: record.context.url.clone(),
        ready_steps: vec![],
        params: None,
        detail: Some(serde_json::json!({
            "status": "queued",
            "concurrency_key": record.concurrency_key,
        })),
    }
}

/// The intent to persist with a start whose admission ended up queued.
pub fn queued_outbox(
    execution_id: &ExecutionId,
    record: &StartRecord,
    admission: Option<&Admission>,
    at_ms: i64,
) -> Option<runbook_engine::NotificationIntent> {
    admission
        .filter(|a| a.state == crate::catalog::AdmissionState::Queued)
        .map(|_| queued_intent(execution_id, record, at_ms))
}

/// Is this lease a start activation?
pub fn is_activation(lease: &WorkLease) -> bool {
    lease.id.kind == WorkKind::Activation
}
