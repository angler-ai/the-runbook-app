//! LANG4 `call`: child executions, through each run's own outbox.
//!
//! A call step crosses executions three times, and each crossing is a
//! durable work item written in the same commit as the fact that needs it,
//! so a crash can neither lose it nor apply it twice:
//!
//! - **Start** (`call-start:<step run>`, on the parent): written with
//!   `CallRequested`. The drain starts the child through the ordinary start
//!   path ([`crate::start::commit_start_with_policy`]): the same admission,
//!   concurrency policy and monthly quota as a top-level start, so a child
//!   counts as one run and fan-out cannot bypass the quota. The child's ID
//!   ([`child_execution_id`]) and idempotency key are derived from the
//!   parent and the step run, so a retried start replays the first one.
//!   The drain then records `CallStarted` (or, when the start was refused,
//!   `CallCompleted` with the refusal code) on the parent, which removes
//!   the item.
//! - **Result** (`call-parent`, on the child): written with the child's
//!   terminal commit. The drain records `CallCompleted` with the child's
//!   status, verdict and outputs on the parent, unless the parent no longer
//!   waits for it.
//! - **Cancel** (`call-cancel:<step run>`, on the parent): present while a
//!   call step that asked for a child has stopped waiting for it without
//!   its outcome (the run was cancelled, failed or timed out). The drain
//!   cancels the child as the system; a child that never started is done.
//!
//! The items are the stores' timer rows: every adapter (memory, Postgres,
//! the ExecutionDO) already derives due work from them, leases it, and
//! keeps an acknowledged item from being offered again. Nothing here holds
//! one run's session while it writes another's.
use crate::{
    CommitResult, StoreError,
    actor::{ActorError, begin_actor, begin_detached},
    blob::BlobStore,
    catalog::{CatalogStore, TenantScope},
    directory::DirectoryStore,
    drain::Processed,
    session::SessionStore,
    start::{StartCommand, StartError, StartStore, commit_start_with_policy, prepare_start_commit},
    work_queue::{WorkLease, WorkQueue},
};
use runbook_engine::{CallOutcome, Event, ExecutionState, ParentLink};
use runbook_model::{
    ActorRef, Data, ExecutionId, ExecutionStatus, StepRunId, StepStatus, Verdict, VersionSelector,
};
use runbook_spec::ir::{MAX_CALL_DEPTH, NodeKind};
use std::collections::BTreeMap;

/// Work item keys (timer rows) for the three crossings.
pub const START_KEY: &str = "call-start:";
pub const CANCEL_KEY: &str = "call-cancel:";
pub const PARENT_KEY: &str = "call-parent";

/// The code a call step fails with when the child would nest deeper than
/// PRD §17 allows (the compiler refuses it first; this is the backstop).
pub const DEPTH_CODE: &str = "call.depth_exceeded";

/// Commits tried against a run that keeps moving before giving up (the
/// lease then expires and the drain tries again).
const ATTEMPTS: usize = 8;

/// The child execution a call step run starts: a stable function of the
/// parent and the step run, so every retry names the same child.
pub fn child_execution_id(parent: &ExecutionId, step_run_id: &StepRunId) -> ExecutionId {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"runbook.call-child.v1\0");
    hash.update(parent.as_str().as_bytes());
    hash.update(b"\0");
    hash.update(step_run_id.as_str().as_bytes());
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    let mut number = u128::from_be_bytes(bytes);
    let mut encoded = [b'0'; 26];
    for byte in encoded.iter_mut().rev() {
        *byte = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[(number & 31) as usize];
        number >>= 5;
    }
    let body: String = encoded.into_iter().map(char::from).collect();
    format!("exe_{body}").parse().expect("128-bit Crockford ID")
}

/// The idempotency key of a call's child start (tenant-scoped like every
/// start key).
pub fn start_key(parent: &ExecutionId, step_run_id: &StepRunId) -> String {
    format!("runbook.call/{parent}/{step_run_id}")
}

fn terminal(status: ExecutionStatus) -> bool {
    matches!(
        status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
}

/// The call work items `state` needs, keyed like timers: generation 1 and
/// a due time that never changes, so later commits leave them alone.
pub(crate) fn desired(state: &ExecutionState, out: &mut BTreeMap<String, (u64, i64)>) {
    let closed = terminal(state.status);
    for step in &state.steps {
        let (Some(id), Some(call)) = (&step.id, &step.call) else {
            continue;
        };
        if call.completed {
            continue;
        }
        let waiting = step.status == StepStatus::Waiting && !closed;
        if waiting && !call.started {
            out.insert(format!("{START_KEY}{id}"), (1, call.requested_at_ms));
        }
        if !waiting {
            out.insert(format!("{CANCEL_KEY}{id}"), (1, call.requested_at_ms));
        }
    }
    if closed && state.parent.is_some() {
        out.insert(PARENT_KEY.into(), (1, state.started_at_ms.unwrap_or(0)));
    }
}

/// Whether a timer key is one of the call work items.
pub fn is_call_key(key: &str) -> bool {
    key.starts_with(START_KEY) || key.starts_with(CANCEL_KEY) || key == PARENT_KEY
}

/// A child's URL: the parent's, naming the child instead.
fn child_url(state: &ExecutionState, child: &ExecutionId) -> String {
    state.url.replace(state.id.as_str(), child.as_str())
}

/// The worst verdict any of the run's steps found (groups already carry
/// their children's), `None` when nothing was checked.
fn run_verdict(state: &ExecutionState) -> Option<Verdict> {
    state
        .steps
        .iter()
        .filter_map(|s| s.verdict)
        .max_by_key(|v| match v {
            Verdict::Pass => 0,
            Verdict::Warn => 1,
            Verdict::Fail => 2,
        })
}

/// Process one call work item lease (`key` is its timer key).
pub async fn process<S, D>(
    store: &S,
    directory: &D,
    scope: TenantScope,
    lease: &WorkLease,
    key: &str,
    generation: u64,
) -> Result<Processed, ActorError>
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore,
    D: DirectoryStore,
{
    let snapshot = directory
        .snapshot(&scope.org_id)
        .await
        .map_err(|_| StoreError::Backend("actor.directory_unavailable"))?;
    let id = &lease.id.execution_id;
    let work = match begin_detached(store, snapshot, scope.clone(), id).await {
        Ok(work) => work,
        Err(ActorError::Store(StoreError::NotFound)) => {
            acknowledge(store, lease).await?;
            return Ok(Processed::Obsolete);
        }
        Err(error) => return Err(error),
    };
    if work.snapshot.timers.get(key).map(|t| t.generation) != Some(generation) {
        drop(work);
        acknowledge(store, lease).await?;
        return Ok(Processed::Obsolete);
    }
    let state = work.runtime.state.clone();
    drop(work);
    let done = if let Some(step) = key.strip_prefix(START_KEY) {
        let step_run_id: StepRunId = step
            .parse()
            .map_err(|_| StoreError::Backend("call.invalid_work"))?;
        start_child(store, directory, &scope, &state, &step_run_id).await?
    } else if let Some(step) = key.strip_prefix(CANCEL_KEY) {
        let step_run_id: StepRunId = step
            .parse()
            .map_err(|_| StoreError::Backend("call.invalid_work"))?;
        let child = child_execution_id(&state.id, &step_run_id);
        cancel_child(store, directory, &scope, &child).await?;
        true
    } else {
        report_to_parent(store, directory, &scope, &state).await?
    };
    if !done {
        return Ok(Processed::Retry);
    }
    acknowledge(store, lease).await?;
    Ok(Processed::Fired)
}

async fn acknowledge<S: WorkQueue>(store: &S, lease: &WorkLease) -> Result<(), StoreError> {
    match store.ack(&lease.id, lease.generation).await {
        Ok(()) | Err(StoreError::InvalidBatch("queue.work_obsolete")) => Ok(()),
        Err(error) => Err(error),
    }
}

/// A start that cannot succeed on retry fails the call step with this code;
/// `None` is a transient failure (the lease is retried). A pool that kept
/// moving or a holder that could not be read (`cancel_previous`) may settle
/// on the next try, so those are transient too.
fn refusal(error: &StartError) -> Option<String> {
    Some(match error {
        StartError::Store(StoreError::InvalidBatch(code)) => (*code).to_owned(),
        StartError::Store(StoreError::NotFound) => "call.child_unavailable".into(),
        StartError::Store(StoreError::Backend(_))
        | StartError::ConcurrencyContended
        | StartError::ConcurrencyHolderUnavailable(_) => return None,
        StartError::InvalidData(_) => StartError::INVALID_DATA_CODE.into(),
        StartError::Diagnostics(_) => "start.invalid".into(),
        other => other.to_string(),
    })
}

/// What starting a call's child came to.
#[derive(Debug, PartialEq, Eq)]
pub enum ChildStart {
    /// The child exists: started now, replayed, or found with this link.
    Started,
    /// The start was refused with this code (the step fails with it).
    Refused(String),
}

/// Whether `child` exists as the child of `parent`'s `step_run_id`.
async fn linked_child<S: StartStore>(
    store: &S,
    child: &ExecutionId,
    parent: &ExecutionId,
    step_run_id: &StepRunId,
) -> Result<bool, StoreError> {
    Ok(store.start_record(child).await?.is_some_and(|record| {
        record
            .context
            .parent
            .is_some_and(|link| &link.execution_id == parent && &link.step_run_id == step_run_id)
    }))
}

/// Commit the start of `step_run_id`'s child as `parent` (a state that asked
/// for it) describes, without recording anything on the parent: the first
/// half of the `call-start` item. A child that already exists with this
/// link counts as started (a retry after the replay's 24-hour retention).
/// Public as a test seam for crash and race cases.
#[doc(hidden)]
pub async fn commit_child_start<S, D>(
    store: &S,
    directory: &D,
    scope: &TenantScope,
    parent: &ExecutionState,
    step_run_id: &StepRunId,
) -> Result<ChildStart, ActorError>
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore,
    D: DirectoryStore,
{
    let child = child_execution_id(&parent.id, step_run_id);
    if linked_child(store, &child, &parent.id, step_run_id).await? {
        return Ok(ChildStart::Started);
    }
    let step = parent
        .steps
        .iter()
        .find(|s| s.id.as_ref() == Some(step_run_id))
        .ok_or(StoreError::Backend("call.invalid_work"))?;
    let Some(NodeKind::Call(call)) = parent.node(&step.step_id).map(|n| &n.kind) else {
        return Err(StoreError::Backend("call.invalid_work").into());
    };
    let target = call
        .target
        .as_ref()
        .ok_or(StoreError::Backend("call.unpinned"))?;
    let depth = parent.parent.as_ref().map_or(0, |p| p.depth) + 1;
    if depth > MAX_CALL_DEPTH {
        return Ok(ChildStart::Refused(DEPTH_CODE.to_owned()));
    }
    let command = StartCommand {
        execution_id: child.clone(),
        runbook_id: target
            .runbook_id
            .parse()
            .map_err(|_| StoreError::Backend("call.unpinned"))?,
        version: Some(VersionSelector::Id(
            target
                .version_id
                .parse()
                .map_err(|_| StoreError::Backend("call.unpinned"))?,
        )),
        inputs: step
            .call
            .as_ref()
            .map(|c| c.inputs.clone())
            .unwrap_or_default(),
        fields: Data::new(),
        project_vars: parent.project_vars.clone(),
        title: None,
        tags: vec![],
        url: child_url(parent, &child),
        created_by: parent.created_by.clone(),
        limits: parent.limits,
        idempotency_key: start_key(&parent.id, step_run_id),
        principal: parent.created_by.clone(),
        request_hash: request_hash(&parent.id, step_run_id, &child),
    };
    let link = ParentLink {
        execution_id: parent.id.clone(),
        step_run_id: step_run_id.clone(),
        step_id: step.step_id.clone(),
        depth,
    };
    let mut commit = match prepare_start_commit(store, store, &command).await {
        Ok(commit) => commit,
        Err(error) => {
            return match refusal(&error) {
                Some(code) => Ok(ChildStart::Refused(code)),
                None => Err(transient(error)),
            };
        }
    };
    commit.record.context.parent = Some(link);
    match commit_start_with_policy(store, directory, scope, commit.clone()).await {
        // A saved start answers a retry without committing. On Cloudflare
        // the saved start may be half-committed (its D1 batch done, the
        // child's object never initialized): commit it again, as the start
        // route does, which replays the D1 half and initializes the object.
        Ok(_) => match store.load(&child).await {
            Err(StoreError::NotFound) => {
                store.commit_start(commit).await?;
                Ok(ChildStart::Started)
            }
            Ok(_) => Ok(ChildStart::Started),
            Err(error) => Err(error.into()),
        },
        Err(error) => {
            // Another worker's start of this very child got there first.
            if linked_child(store, &child, &parent.id, step_run_id).await? {
                return Ok(ChildStart::Started);
            }
            match refusal(&error) {
                Some(code) => Ok(ChildStart::Refused(code)),
                None => Err(transient(error)),
            }
        }
    }
}

/// Start the child of `step_run_id` and record the result on the parent,
/// as the `call-start` item does from the parent state it read (`parent`).
/// `false` when the parent kept changing (the lease is retried). Public as
/// a test seam for overlapping leases.
#[doc(hidden)]
pub async fn start_child<S, D>(
    store: &S,
    directory: &D,
    scope: &TenantScope,
    parent: &ExecutionState,
    step_run_id: &StepRunId,
) -> Result<bool, ActorError>
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore,
    D: DirectoryStore,
{
    let child = child_execution_id(&parent.id, step_run_id);
    if !runbook_engine::awaits_start(parent, step_run_id, &child) {
        return Ok(true);
    }
    let started = commit_child_start(store, directory, scope, parent, step_run_id).await?;
    record_child_start(store, directory, scope, &parent.id, step_run_id, started).await
}

/// Record what starting `step_run_id`'s child came to on the parent: the
/// second half of the `call-start` item. `false` when the parent kept
/// changing (the lease is retried). Public as a test seam for overlapping
/// leases.
#[doc(hidden)]
pub async fn record_child_start<S, D>(
    store: &S,
    directory: &D,
    scope: &TenantScope,
    parent: &ExecutionId,
    step_run_id: &StepRunId,
    started: ChildStart,
) -> Result<bool, ActorError>
where
    S: StartStore + BlobStore + SessionStore,
    D: DirectoryStore,
{
    let child = child_execution_id(parent, step_run_id);
    match started {
        ChildStart::Started => {
            let event = Event::CallStarted {
                step_run_id: step_run_id.clone(),
                child_execution_id: child.clone(),
            };
            match record(
                store,
                directory,
                scope,
                parent,
                step_run_id,
                &child,
                true,
                event,
            )
            .await?
            {
                // Another worker (an overlapping lease) recorded the start:
                // the child is wanted.
                Recorded::Applied | Recorded::AlreadyThere => Ok(true),
                Recorded::Contended => Ok(false),
                // The step no longer awaits this child's outcome: the run
                // was cancelled meanwhile, or the step already took an
                // outcome (the child's own result, or another worker's
                // refusal). The child is cancelled; a finished one is left
                // as it is (re-check 3).
                Recorded::NotWaiting => {
                    cancel_child(store, directory, scope, &child).await?;
                    Ok(true)
                }
            }
        }
        ChildStart::Refused(code) => {
            let event = Event::CallCompleted {
                outcome: Box::new(CallOutcome {
                    step_run_id: step_run_id.clone(),
                    child_execution_id: child.clone(),
                    status: ExecutionStatus::Failed,
                    verdict: None,
                    outputs: Data::new(),
                    error: Some(code),
                }),
            };
            Ok(record(
                store,
                directory,
                scope,
                parent,
                step_run_id,
                &child,
                false,
                event,
            )
            .await?
                != Recorded::Contended)
        }
    }
}

fn transient(error: StartError) -> ActorError {
    match error {
        StartError::Store(error) => ActorError::Store(error),
        _ => ActorError::Store(StoreError::Backend("call.start_failed")),
    }
}

fn request_hash(parent: &ExecutionId, step_run_id: &StepRunId, child: &ExecutionId) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"runbook.call-start.v1\0");
    for part in [parent.as_str(), step_run_id.as_str(), child.as_str()] {
        hash.update(part.as_bytes());
        hash.update(b"\0");
    }
    hash.finalize().into()
}

#[derive(Debug, PartialEq, Eq)]
enum Recorded {
    Applied,
    /// The start is already recorded (by another worker): the step awaits
    /// this child's outcome, so the child is wanted.
    AlreadyThere,
    /// The step no longer awaits this child's outcome (cancelled, finished,
    /// or the parent is gone).
    NotWaiting,
    /// The parent kept moving under every attempt.
    Contended,
}

/// Append `event` to the parent in its own session, if it still waits:
/// for the child's start (`start`) or its outcome.
#[allow(clippy::too_many_arguments)]
async fn record<S, D>(
    store: &S,
    directory: &D,
    scope: &TenantScope,
    parent: &ExecutionId,
    step_run_id: &StepRunId,
    child: &ExecutionId,
    start: bool,
    event: Event,
) -> Result<Recorded, ActorError>
where
    S: StartStore + BlobStore + SessionStore,
    D: DirectoryStore,
{
    for _ in 0..ATTEMPTS {
        let mut work = match begin_actor(store, directory, scope.clone(), parent).await {
            Ok(work) => work,
            // The parent was deleted: nobody waits.
            Err(ActorError::Store(StoreError::NotFound)) => return Ok(Recorded::NotWaiting),
            Err(error) => return Err(error),
        };
        let state = &work.runtime.state;
        let outcome = runbook_engine::awaits_outcome(state, step_run_id, child);
        if start && outcome && !runbook_engine::awaits_start(state, step_run_id, child) {
            return Ok(Recorded::AlreadyThere);
        }
        let waits = if start {
            runbook_engine::awaits_start(state, step_run_id, child)
        } else {
            outcome
        };
        if !waits {
            return Ok(Recorded::NotWaiting);
        }
        work.append(event.clone(), ActorRef::System)?;
        work.settle()?;
        match work.commit(vec![]).await? {
            CommitResult::Committed(_) => return Ok(Recorded::Applied),
            CommitResult::Conflict => continue,
        }
    }
    Ok(Recorded::Contended)
}

/// A run's current state from its stored log, read without a session (no
/// lock is held). `None` when the run does not exist.
pub async fn stored_state<S: StartStore + BlobStore>(
    store: &S,
    id: &ExecutionId,
) -> Result<Option<ExecutionState>, StoreError> {
    let run = match store.load(id).await {
        Ok(run) => run,
        Err(StoreError::NotFound) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(record) = store.start_record(id).await? else {
        return Ok(None);
    };
    let bytes = store
        .get(&crate::blob::BlobKey::Shared {
            hash: record.snapshot_digest,
        })
        .await?;
    let compiled = runbook_spec::restore(&bytes)
        .map_err(|_| StoreError::Backend("start.snapshot_restore_failed"))?;
    let mut state = ExecutionState::new(id.clone(), std::rc::Rc::new(compiled));
    for event in &run.events {
        state = runbook_engine::apply(&state, event)
            .map_err(|_| StoreError::Backend("execution.events_invalid"))?;
    }
    Ok(Some(state))
}

/// Whether the call step named by a child's parent link still awaits this
/// child's outcome. A child that is no longer wanted (its parent was
/// cancelled, failed or deleted while it was being started) never runs.
pub async fn wanted<S: StartStore + BlobStore>(
    store: &S,
    child: &ExecutionId,
    link: &ParentLink,
) -> Result<bool, StoreError> {
    Ok(stored_state(store, &link.execution_id)
        .await?
        .is_some_and(|parent| runbook_engine::awaits_outcome(&parent, &link.step_run_id, child)))
}

/// Whether `id` is a child whose parent still awaits its outcome (its
/// result has not reached the parent yet). Retention and deletion leave
/// such a run alone until it has.
pub async fn result_pending<S: StartStore + BlobStore>(
    store: &S,
    id: &ExecutionId,
) -> Result<bool, StoreError> {
    match store.start_record(id).await? {
        Some(record) => match &record.context.parent {
            Some(link) => wanted(store, id, link).await,
            None => Ok(false),
        },
        None => Ok(false),
    }
}

/// Whether closed run `id` still has to cancel a child: a call step that
/// stopped waiting without its child's outcome, while that child is still
/// open (its `call-cancel` item has not run yet). Deleting the run now
/// would leave the child running with nobody to cancel it.
pub async fn child_cancel_pending<S: StartStore + BlobStore>(
    store: &S,
    id: &ExecutionId,
) -> Result<bool, StoreError> {
    let Some(state) = stored_state(store, id).await? else {
        return Ok(false);
    };
    let closed = terminal(state.status);
    for step in &state.steps {
        let Some(call) = &step.call else { continue };
        if call.completed || (step.status == StepStatus::Waiting && !closed) {
            continue;
        }
        match store.load(&call.child_execution_id).await {
            Ok(child) if !terminal(child.projection.status) => return Ok(true),
            Ok(_) | Err(StoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// Cancel `child` as the system. A child that never started (or is gone)
/// needs nothing; a finished one is left as it is.
pub(crate) async fn cancel_child<S, D>(
    store: &S,
    directory: &D,
    scope: &TenantScope,
    child: &ExecutionId,
) -> Result<(), ActorError>
where
    S: StartStore + BlobStore + SessionStore,
    D: DirectoryStore,
{
    for _ in 0..ATTEMPTS {
        let mut work = match begin_actor(store, directory, scope.clone(), child).await {
            Ok(work) => work,
            Err(ActorError::Store(StoreError::NotFound)) => return Ok(()),
            Err(error) => return Err(error),
        };
        crate::control::prepare_cancel_as(&mut work, ActorRef::System, None)?;
        if !work.has_pending() {
            return Ok(());
        }
        work.settle()?;
        match work.commit(vec![]).await? {
            CommitResult::Committed(_) => return Ok(()),
            CommitResult::Conflict => continue,
        }
    }
    Err(StoreError::Backend("call.cancel_contended").into())
}

/// The child's terminal outcome, to its parent. `false` when the parent
/// kept changing (the lease is retried).
async fn report_to_parent<S, D>(
    store: &S,
    directory: &D,
    scope: &TenantScope,
    child: &ExecutionState,
) -> Result<bool, ActorError>
where
    S: StartStore + BlobStore + SessionStore,
    D: DirectoryStore,
{
    let Some(link) = &child.parent else {
        return Ok(true);
    };
    if !terminal(child.status) {
        return Ok(true);
    }
    let event = Event::CallCompleted {
        outcome: Box::new(CallOutcome {
            step_run_id: link.step_run_id.clone(),
            child_execution_id: child.id.clone(),
            status: child.status,
            verdict: run_verdict(child),
            outputs: child.outputs.clone(),
            error: None,
        }),
    };
    Ok(record(
        store,
        directory,
        scope,
        &link.execution_id,
        &link.step_run_id,
        &child.id,
        false,
        event,
    )
    .await?
        != Recorded::Contended)
}

/// LANG4: what a compile context needs about each runbook `source` calls
/// (keyed by [`runbook_spec::calls::call_key`]), read from the project's
/// catalog: the pinned or latest published version of each named runbook
/// and what its compiled snapshot says about its own calls and inputs. A
/// reference that does not resolve (no such runbook in `runbooks`, an
/// archived one, no such published version) is left out, so compiling
/// reports it (`spec.call_unresolved`). A source that does not parse calls
/// nothing.
pub async fn call_targets<C: CatalogStore + BlobStore>(
    catalog: &C,
    runbooks: &[crate::catalog::RunbookRecord],
    source: &str,
) -> Result<BTreeMap<String, runbook_spec::ir::CallResolution>, StoreError> {
    let Ok(ir) = runbook_spec::parse(source).and_then(|spec| runbook_spec::normalize(&spec)) else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for (slug, version) in runbook_spec::calls::references(&ir) {
        let Some(record) = runbooks.iter().find(|r| r.slug == slug && !r.archived) else {
            continue;
        };
        let selector = version
            .and_then(std::num::NonZeroU32::new)
            .map(VersionSelector::Number);
        let view = match catalog.resolve_version(&record.runbook_id, selector).await {
            Ok(view) => view,
            Err(StoreError::NotFound) => continue,
            Err(error) => return Err(error),
        };
        let Some(number) = view.runbook_version else {
            continue;
        };
        let Some(stored) = catalog
            .version_record(&record.runbook_id, &view.runbook_version_id)
            .await?
        else {
            continue;
        };
        let bytes = catalog
            .get(&crate::blob::BlobKey::Shared {
                hash: stored.snapshot_digest,
            })
            .await?;
        let compiled = runbook_spec::restore(&bytes)
            .map_err(|_| StoreError::Backend("start.snapshot_restore_failed"))?;
        out.insert(
            runbook_spec::calls::call_key(&slug, version),
            compiled.call_resolution(
                record.runbook_id.as_str(),
                view.runbook_version_id.as_str(),
                number.get(),
            ),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_ids_are_stable_and_distinct() {
        let parent: ExecutionId = "exe_00000000000000000000000001".parse().unwrap();
        let a: StepRunId = "sr_00000000000000000000000001".parse().unwrap();
        let b: StepRunId = "sr_00000000000000000000000002".parse().unwrap();
        assert_eq!(
            child_execution_id(&parent, &a),
            child_execution_id(&parent, &a)
        );
        assert_ne!(
            child_execution_id(&parent, &a),
            child_execution_id(&parent, &b)
        );
        assert_ne!(child_execution_id(&parent, &a), parent);
    }
}
