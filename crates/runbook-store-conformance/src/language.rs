//! Language completeness store tests (plan §2.9, later-plans rule 1): each
//! key that moved from rejected to supported runs end to end through the
//! core use cases on every adapter, and the committed log replays.
use crate::portable::{agent, directory, exe, p, start};
use runbook_core::{
    actor::begin_actor,
    claims::{claim_step, report_step},
};
use runbook_model::{ClaimRequest, ReportRequest, ReportStatus, StepRunId, StepStatus, Verdict};

/// Every bound a language store test needs.
pub trait LanguageStore:
    runbook_core::start::StartStore
    + runbook_core::catalog::CatalogStore
    + runbook_core::work_queue::WorkQueue
    + runbook_core::session::SessionStore
    + runbook_core::ExecutionStore
    + runbook_core::blob::BlobStore
{
}
impl<T> LanguageStore for T where
    T: runbook_core::start::StartStore
        + runbook_core::catalog::CatalogStore
        + runbook_core::work_queue::WorkQueue
        + runbook_core::session::SessionStore
        + runbook_core::ExecutionStore
        + runbook_core::blob::BlobStore
{
}
pub trait LanguageDirectory:
    runbook_core::directory::DirectoryStore + runbook_core::directory_operations::DirectoryOperations
{
}
impl<T> LanguageDirectory for T where
    T: runbook_core::directory::DirectoryStore
        + runbook_core::directory_operations::DirectoryOperations
{
}

async fn step_run<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    n: u8,
    step_id: &str,
) -> Option<StepRunId> {
    begin_actor(store, dir, p(), &exe(n))
        .await
        .unwrap()
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == step_id)
        .and_then(|s| s.id.clone())
}

/// Claim one agent task of run `n` and report it succeeded with `outputs`.
pub(crate) async fn report<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    n: u8,
    step_id: &str,
    claim: u8,
    verdict: Option<Verdict>,
    outputs: serde_json::Value,
) {
    let id = step_run(store, dir, n, step_id)
        .await
        .unwrap_or_else(|| panic!("{step_id} is not materialized"));
    let work = begin_actor(store, dir, p(), &exe(n)).await.unwrap();
    let claim = claim_step(
        work,
        &agent(),
        &id,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        format!("clm_{claim:026}").parse().unwrap(),
        None,
    )
    .await
    .unwrap();
    let work = begin_actor(store, dir, p(), &exe(n)).await.unwrap();
    report_step(
        work,
        &agent(),
        &ReportRequest {
            step_run_id: id,
            claim,
            status: ReportStatus::Succeeded,
            verdict,
            outputs: serde_json::from_value(outputs).unwrap(),
            summary: None,
            evidence: vec![],
            logs: vec![],
            cost: None,
        },
        None,
    )
    .await
    .unwrap();
}

const LOOP_WHILE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: poll, title: Poll}\n\
    spec:\n  defaults: {actor: agent}\n  steps:\n\
    \x20   - id: poll\n      loop:\n        while: \"${{ !steps.probe.outputs.healthy }}\"\n\
    \x20       max_iterations: 3\n        steps:\n\
    \x20         - {id: probe, title: Probe, check: true, outputs: {healthy: {type: boolean, required: true}}}\n";

/// LANG1 store test: a `loop.while` repeats while its condition holds and
/// stops when it is false; the stored log replays to the same loop facts.
pub async fn loop_while_runs_and_replays<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    crate::parity::publish(store, 3, "poll", LOOP_WHILE, 3).await;
    start(store, dir, &p(), 1, 3).await;
    report(
        store,
        dir,
        1,
        "poll#1.probe",
        1,
        Some(Verdict::Fail),
        serde_json::json!({"healthy": false}),
    )
    .await;
    assert!(step_run(store, dir, 1, "poll#2.probe").await.is_some());
    assert!(step_run(store, dir, 1, "poll#3.probe").await.is_none());
    report(
        store,
        dir,
        1,
        "poll#2.probe",
        2,
        Some(Verdict::Pass),
        serde_json::json!({"healthy": true}),
    )
    .await;
    let state = begin_actor(store, dir, p(), &exe(1))
        .await
        .unwrap()
        .runtime
        .state;
    let poll = state.steps.iter().find(|s| s.step_id == "poll").unwrap();
    assert_eq!(poll.status, StepStatus::Succeeded);
    assert!(poll.satisfied);
    assert!(!poll.exhausted);
    assert_eq!(poll.verdict, Some(Verdict::Fail));
    assert!(state.steps.iter().all(|s| s.step_id != "poll#3.probe"));
    assert_eq!(
        crate::portable::status(store, 1).await.unwrap(),
        runbook_model::ExecutionStatus::Succeeded
    );
}

const JOIN_ANY: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: race, title: Race}\n\
    spec:\n  defaults: {actor: agent}\n  steps:\n\
    \x20   - id: mirrors\n      join: any\n      parallel:\n\
    \x20       - {id: east, title: East}\n        - {id: west, title: West}\n\
    \x20   - {id: after, title: After}\n";

/// LANG2 store test: a `join: any` parallel completes on its first success
/// and cancels the open branch, a claimed one included; the late report on
/// the cancelled branch is refused, and the stored log replays.
pub async fn parallel_join_cancels_open_branches<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    crate::parity::publish(store, 3, "race", JOIN_ANY, 3).await;
    start(store, dir, &p(), 1, 3).await;
    // Claim the west branch first: it is open, claimed work when cancelled.
    let west = step_run(store, dir, 1, "west").await.unwrap();
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let west_claim = claim_step(
        work,
        &agent(),
        &west,
        &ClaimRequest {
            take_over: false,
            session_id: "s2".into(),
            actor: None,
        },
        "clm_00000000000000000000000009".parse().unwrap(),
        None,
    )
    .await
    .unwrap();
    report(store, dir, 1, "east", 1, None, serde_json::json!({})).await;
    let state = begin_actor(store, dir, p(), &exe(1))
        .await
        .unwrap()
        .runtime
        .state;
    let status = |id: &str| state.steps.iter().find(|s| s.step_id == id).unwrap().status;
    assert_eq!(status("east"), StepStatus::Succeeded);
    assert_eq!(status("west"), StepStatus::Cancelled);
    assert_eq!(status("mirrors"), StepStatus::Succeeded);
    assert_eq!(status("after"), StepStatus::Ready);
    // The agent still holding west learns on its next report.
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let late = report_step(
        work,
        &agent(),
        &ReportRequest {
            step_run_id: west,
            claim: west_claim,
            status: ReportStatus::Succeeded,
            verdict: None,
            outputs: Default::default(),
            summary: None,
            evidence: vec![],
            logs: vec![],
            cost: None,
        },
        None,
    )
    .await;
    assert_eq!(late.unwrap_err().code(), "claim.stale_generation");
    report(store, dir, 1, "after", 2, None, serde_json::json!({})).await;
    assert_eq!(
        crate::portable::status(store, 1).await.unwrap(),
        runbook_model::ExecutionStatus::Succeeded
    );
}

// LANG3: `concurrency.on_conflict`.

fn runbook_source(id: &str, limit: u32, policy: &str, finally: bool) -> String {
    let finally = if finally {
        "  finally: [{id: cleanup, title: Clean up}]\n"
    } else {
        ""
    };
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: {id}, title: {id}}}\n\
         spec:\n  defaults: {{actor: agent}}\n  execution: {{concurrency: {{key: prod, limit: {limit}, on_conflict: {policy}}}}}\n\
         \x20 steps: [{{id: work, title: Work}}]\n{finally}"
    )
}

fn start_command(n: u8, runbook: u8) -> runbook_core::start::StartCommand {
    runbook_core::start::StartCommand {
        execution_id: exe(n),
        runbook_id: crate::portable::rb(runbook),
        version: None,
        inputs: Default::default(),
        fields: Default::default(),
        project_vars: Default::default(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: format!("user:{}", crate::portable::user(1)),
        limits: Default::default(),
        idempotency_key: format!("start-{n}"),
        principal: format!("user:{}", crate::portable::user(1)),
        request_hash: [n; 32],
    }
}

/// Start run `n` of `runbook` in `scope` under its conflict policy, then
/// activate every admitted, unstarted run (D24).
async fn start_in<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    scope: &runbook_core::catalog::TenantScope,
    n: u8,
    runbook: u8,
) -> Result<runbook_core::start::StartOutcome, runbook_core::start::StartError> {
    let commit =
        runbook_core::start::prepare_start_commit(store, store, &start_command(n, runbook))
            .await
            .unwrap();
    let outcome = runbook_core::start::commit_start_with_policy(store, dir, scope, commit).await?;
    activate_due(store, dir, scope).await;
    Ok(outcome)
}
async fn activate_due<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    scope: &runbook_core::catalog::TenantScope,
) {
    for lease in store.claim_due(64, 60_000).await.unwrap() {
        if runbook_core::start::is_activation(&lease) {
            runbook_core::start::activate(store, dir, scope.clone(), &lease)
                .await
                .unwrap();
        }
    }
}
async fn start_with_policy<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    n: u8,
    runbook: u8,
) -> Result<runbook_core::start::StartOutcome, runbook_core::start::StartError> {
    start_in(store, dir, &p(), n, runbook).await
}

async fn admission_state<S: LanguageStore>(
    store: &S,
    n: u8,
) -> Option<runbook_core::catalog::AdmissionState> {
    store.admission(&exe(n)).await.unwrap().map(|a| a.state)
}
async fn run_state<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    n: u8,
) -> runbook_engine::ExecutionState {
    begin_actor(store, dir, p(), &exe(n))
        .await
        .unwrap()
        .runtime
        .state
}
async fn event_count<S: LanguageStore>(store: &S, n: u8) -> usize {
    store.load(&exe(n)).await.unwrap().events.len()
}
fn code(error: &runbook_core::start::StartError) -> String {
    error.to_string()
}

/// LANG3 store test: `on_conflict: reject` refuses a start into a full pool
/// with `execution.concurrency_conflict` and writes nothing, so the same
/// idempotency key succeeds once the holder has finished; a retry of that
/// start replays it.
pub async fn on_conflict_reject_refuses_a_full_pool<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    use runbook_core::catalog::AdmissionState;
    directory(dir).await;
    crate::parity::publish(
        store,
        3,
        "guarded",
        &runbook_source("guarded", 1, "reject", false),
        3,
    )
    .await;
    let first = start_with_policy(store, dir, 1, 3).await.unwrap();
    assert_eq!(
        admission_state(store, 1).await,
        Some(AdmissionState::Admitted)
    );
    // A retried start (same key) replays and refuses nothing.
    assert_eq!(start_with_policy(store, dir, 1, 3).await.unwrap(), first);
    let refused = start_with_policy(store, dir, 2, 3).await.unwrap_err();
    assert_eq!(code(&refused), runbook_core::concurrency::CONFLICT_CODE);
    assert!(matches!(
        store.load(&exe(2)).await,
        Err(runbook_core::StoreError::NotFound)
    ));
    assert_eq!(admission_state(store, 2).await, None);
    assert!(store.find_replay("start-2").await.unwrap().is_none());
    // The holder is untouched.
    assert_eq!(
        crate::portable::status(store, 1).await.unwrap(),
        runbook_model::ExecutionStatus::Running
    );
    // Once it finishes, the same key starts the run.
    report(store, dir, 1, "work", 1, None, serde_json::json!({})).await;
    assert_eq!(
        admission_state(store, 1).await,
        Some(AdmissionState::Released)
    );
    start_with_policy(store, dir, 2, 3).await.unwrap();
    assert_eq!(
        admission_state(store, 2).await,
        Some(AdmissionState::Admitted)
    );
}

/// LANG3 store test: `on_conflict: cancel_previous` with `limit: 1`.
///
/// - A start into a full pool cancels its holder (a `queue` runbook's run
///   with a claimed step) and every waiter, and records who replaced them.
///   The holder has no cleanup left, so its slot frees in the same commit
///   and the new run is admitted at once.
/// - A holder with a `finally` keeps its slot while it cleans up (mutual
///   exclusion holds); the new run waits first in line, ahead of a waiter
///   that arrives later, and starts when the holder closes.
/// - A retried start (same key) replays and cancels nothing; a start
///   prepared against another pool is stale and writes nothing.
///
/// Returns the cancelled runs' logs and the admission states as a trace.
pub async fn on_conflict_cancel_previous_replaces_the_holders<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) -> serde_json::Value {
    use runbook_core::{catalog::AdmissionState as A, concurrency};
    use runbook_model::ExecutionStatus as X;
    directory(dir).await;
    let queue = runbook_source("deploy", 1, "queue", false);
    let cancel = runbook_source("latest", 1, "cancel_previous", true);
    crate::parity::publish(store, 3, "deploy", &queue, 3).await;
    crate::parity::publish(store, 4, "latest", &cancel, 4).await;
    // exe_1 holds the key with a claimed step; exe_2 waits behind it.
    start_with_policy(store, dir, 1, 3).await.unwrap();
    let work = step_run(store, dir, 1, "work").await.unwrap();
    let session = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    claim_step(
        session,
        &agent(),
        &work,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    )
    .await
    .unwrap();
    start_with_policy(store, dir, 2, 3).await.unwrap();
    assert_eq!(admission_state(store, 2).await, Some(A::Queued));
    // exe_3 (cancel_previous) replaces both; exe_1 had no cleanup left.
    let outcome = start_with_policy(store, dir, 3, 4).await.unwrap();
    assert_eq!(outcome.admission.map(|a| a.state), Some(A::Admitted));
    assert_eq!(
        crate::portable::status(store, 1).await.unwrap(),
        X::Cancelled
    );
    assert_eq!(
        crate::portable::status(store, 2).await.unwrap(),
        X::Cancelled
    );
    assert_eq!(admission_state(store, 1).await, Some(A::Released));
    assert_eq!(admission_state(store, 2).await, Some(A::Released));
    assert_ne!(crate::portable::status(store, 3).await.unwrap(), X::Queued);
    // The trail: who replaced them, on the run and in its report.
    let starter = format!("user:{}", crate::portable::user(1));
    for n in [1, 2] {
        let state = run_state(store, dir, n).await;
        let superseded = state.superseded.clone().expect("trail");
        assert_eq!(superseded.execution_id, exe(3));
        assert_eq!(superseded.started_by, starter);
        let document =
            runbook_core::execution_document::build_document(&state, header(&state), |s| {
                format!("/v1/step-runs/{s}/logs")
            });
        assert_eq!(
            document
                .header
                .superseded_by
                .as_ref()
                .map(|s| &s.execution_id),
            Some(&exe(3))
        );
        let names = [(crate::portable::user(1), "Alice".to_owned())].into();
        let markdown =
            runbook_core::execution_document::render_markdown_named(&state, &document, &names);
        assert!(
            markdown.contains(&format!(
                "Cancelled: replaced by {} started by Alice",
                runbook_core::execution_document::escape(exe(3).as_str())
            )),
            "{markdown}"
        );
    }
    // The cancelled holder's claim was released with its step.
    let state = run_state(store, dir, 1).await;
    let step = state.steps.iter().find(|s| s.step_id == "work").unwrap();
    assert_eq!(step.status, StepStatus::Cancelled);
    // exe_4 replaces exe_3, which has a `finally`: exe_3 keeps the slot
    // while it cleans up, and exe_4 waits first in line.
    let four = start_with_policy(store, dir, 4, 4).await.unwrap();
    assert_eq!(four.admission.as_ref().map(|a| a.state), Some(A::Queued));
    assert_eq!(admission_state(store, 3).await, Some(A::Admitted));
    assert_eq!(admission_state(store, 4).await, Some(A::Queued));
    let three = store.load(&exe(3)).await.unwrap().projection;
    assert_eq!(three.phase, runbook_engine::ExecutionPhase::Cleanup);
    assert_eq!(three.status, X::Running);
    assert_eq!(crate::portable::status(store, 4).await.unwrap(), X::Queued);
    // A retried start (same key) replays its outcome and cancels nothing.
    let events = event_count(store, 3).await;
    assert_eq!(start_with_policy(store, dir, 4, 4).await.unwrap(), four);
    assert_eq!(event_count(store, 3).await, events);
    assert_eq!(admission_state(store, 4).await, Some(A::Queued));
    // A `queue` start arriving now waits behind exe_4.
    start_with_policy(store, dir, 5, 3).await.unwrap();
    assert_eq!(admission_state(store, 5).await, Some(A::Queued));
    assert_eq!(
        store
            .admission_pool("prod")
            .await
            .unwrap()
            .holders
            .iter()
            .map(|h| (h.execution_id.clone(), h.state))
            .collect::<Vec<_>>(),
        [
            (exe(3), A::Admitted),
            (exe(4), A::Queued),
            (exe(5), A::Queued)
        ]
    );
    // exe_3's cleanup ends: the slot passes to exe_4, not exe_5.
    report(store, dir, 3, "cleanup", 3, None, serde_json::json!({})).await;
    assert_eq!(
        crate::portable::status(store, 3).await.unwrap(),
        X::Cancelled
    );
    assert_eq!(admission_state(store, 3).await, Some(A::Released));
    assert_eq!(admission_state(store, 4).await, Some(A::Admitted));
    assert_eq!(admission_state(store, 5).await, Some(A::Queued));
    activate_due(store, dir, &p()).await;
    assert_ne!(crate::portable::status(store, 4).await.unwrap(), X::Queued);
    // A start prepared against another pool is stale: nothing is written.
    let mut commit = runbook_core::start::prepare_start_commit(store, store, &start_command(6, 4))
        .await
        .unwrap();
    commit.on_conflict = Some(concurrency::OnConflict::CancelPrevious {
        view: concurrency::PoolView::default(),
        commits: vec![],
    });
    assert!(matches!(
        runbook_core::start::StartStore::commit_start(store, commit).await,
        Err(runbook_core::StoreError::InvalidBatch(
            concurrency::STALE_CODE
        ))
    ));
    assert!(matches!(
        store.load(&exe(6)).await,
        Err(runbook_core::StoreError::NotFound)
    ));
    // The trace: what the evicted runs recorded, and the final pool.
    let mut logs = serde_json::Map::new();
    for n in [1, 2, 3] {
        let events: Vec<_> = store
            .load(&exe(n))
            .await
            .unwrap()
            .events
            .into_iter()
            .map(|e| serde_json::json!({"seq": e.seq, "actor": e.actor, "event": e.event}))
            .filter(|e| {
                matches!(
                    e["event"]["type"].as_str(),
                    Some(
                        "CancellationRequested"
                            | "ExecutionCancelledBeforeStart"
                            | "ExecutionCompleted"
                    )
                )
            })
            .map(|mut e| {
                if let Some(event) = e["event"].as_object_mut() {
                    event.remove("data");
                }
                e
            })
            .collect();
        logs.insert(exe(n).to_string(), serde_json::Value::Array(events));
    }
    let mut admissions = serde_json::Map::new();
    for n in [1, 2, 3, 4, 5] {
        admissions.insert(
            exe(n).to_string(),
            serde_json::to_value(admission_state(store, n).await).unwrap(),
        );
    }
    serde_json::json!({
        "format": "store-trace-v2",
        "runbooks": {"queue": queue, "cancel_previous": cancel},
        "steps": [
            "exe_1 (queue) admitted, its step claimed; exe_2 (queue) queued",
            "exe_3 (cancel_previous) cancels exe_1 and exe_2; exe_1 closes at once, so exe_3 is admitted",
            "exe_4 (cancel_previous) cancels exe_3, which keeps its slot while its finally runs; exe_4 waits first in line",
            "exe_5 (queue) queues behind exe_4",
            "exe_3's finally ends: exe_4 is admitted, exe_5 still waits",
        ],
        "events": logs,
        "admissions": admissions,
    })
}

/// LANG3 decision 6: with `limit > 1`, `cancel_previous` cancels the oldest
/// holder (admission order) and every waiter ahead of it; younger holders
/// keep running.
pub async fn on_conflict_cancel_previous_with_a_wider_pool_cancels_the_oldest_holder<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) {
    use runbook_core::catalog::AdmissionState as A;
    use runbook_model::ExecutionStatus as X;
    directory(dir).await;
    crate::parity::publish(
        store,
        3,
        "deploy",
        &runbook_source("deploy", 2, "queue", false),
        3,
    )
    .await;
    crate::parity::publish(
        store,
        4,
        "latest",
        &runbook_source("latest", 2, "cancel_previous", false),
        4,
    )
    .await;
    for n in [1, 2, 3] {
        start_with_policy(store, dir, n, 3).await.unwrap();
    }
    assert_eq!(admission_state(store, 3).await, Some(A::Queued));
    let outcome = start_with_policy(store, dir, 4, 4).await.unwrap();
    assert_eq!(outcome.admission.map(|a| a.state), Some(A::Admitted));
    assert_eq!(
        crate::portable::status(store, 1).await.unwrap(),
        X::Cancelled
    );
    assert_eq!(crate::portable::status(store, 2).await.unwrap(), X::Running);
    assert_eq!(
        crate::portable::status(store, 3).await.unwrap(),
        X::Cancelled
    );
    assert_eq!(admission_state(store, 2).await, Some(A::Admitted));
}

/// LANG3: the pool is per key in one project; the same key in another
/// project is a different pool and is never touched.
pub async fn on_conflict_cancel_previous_leaves_other_projects_alone<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    other: &S,
    dir: &D,
) {
    use runbook_core::catalog::AdmissionState as A;
    use runbook_model::ExecutionStatus as X;
    directory(dir).await;
    let cancel = runbook_source("latest", 1, "cancel_previous", false);
    crate::parity::publish(store, 4, "latest", &cancel, 4).await;
    crate::parity::publish(other, 5, "latest", &cancel, 5).await;
    let q = crate::portable::q();
    start_in(other, dir, &q, 9, 5).await.unwrap();
    start_with_policy(store, dir, 1, 4).await.unwrap();
    start_with_policy(store, dir, 2, 4).await.unwrap();
    assert_eq!(
        crate::portable::status(store, 1).await.unwrap(),
        X::Cancelled
    );
    assert_eq!(crate::portable::status(other, 9).await.unwrap(), X::Running);
    assert_eq!(
        other.admission(&exe(9)).await.unwrap().map(|a| a.state),
        Some(A::Admitted)
    );
    assert_eq!(other.admission_pool("prod").await.unwrap().holders.len(), 1);
}

/// LANG3: one start prepares at most 50 cancellations
/// (`execution.concurrency_too_many_holders`), and a holder that cannot be
/// prepared refuses the start with `execution.concurrency_holder_unavailable`
/// naming it; neither writes anything.
pub async fn on_conflict_cancel_previous_bounds_its_preparation<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) {
    use runbook_core::start::StartError;
    directory(dir).await;
    crate::parity::publish(
        store,
        3,
        "deploy",
        &runbook_source("deploy", 1, "queue", false),
        3,
    )
    .await;
    crate::parity::publish(
        store,
        4,
        "latest",
        &runbook_source("latest", 1, "cancel_previous", false),
        4,
    )
    .await;
    // One holder and 50 waiters: 51 cancellations would be needed.
    for n in 1..=51 {
        start_with_policy(store, dir, n, 3).await.unwrap();
    }
    let events = event_count(store, 1).await;
    let refused = start_with_policy(store, dir, 60, 4).await.unwrap_err();
    assert!(
        matches!(refused, StartError::ConcurrencyTooManyHolders),
        "{refused:?}"
    );
    assert_eq!(code(&refused), "execution.concurrency_too_many_holders");
    assert!(matches!(
        store.load(&exe(60)).await,
        Err(runbook_core::StoreError::NotFound)
    ));
    assert_eq!(event_count(store, 1).await, events);
    assert_eq!(
        crate::portable::status(store, 1).await.unwrap(),
        runbook_model::ExecutionStatus::Running
    );
    // Drain the waiters: cancel 2..=51 the ordinary way.
    for n in 2..=51 {
        let work = begin_actor(store, dir, p(), &exe(n)).await.unwrap();
        runbook_core::control::cancel(store, work, &crate::portable::human(1), None)
            .await
            .unwrap();
    }
    // A holder whose pinned snapshot cannot be read cannot be prepared.
    let record = store.start_record(&exe(1)).await.unwrap().unwrap();
    store
        .delete(&runbook_core::blob::BlobKey::Shared {
            hash: record.snapshot_digest,
        })
        .await
        .unwrap();
    let refused = start_with_policy(store, dir, 61, 4).await.unwrap_err();
    assert!(
        matches!(&refused, StartError::ConcurrencyHolderUnavailable(id) if *id == exe(1)),
        "{refused:?}"
    );
    assert_eq!(code(&refused), "execution.concurrency_holder_unavailable");
    assert!(matches!(
        store.load(&exe(61)).await,
        Err(runbook_core::StoreError::NotFound)
    ));
}

/// A run view built from state the way the routes do (the trail is a state
/// fact, applied by `build_document`).
fn header(state: &runbook_engine::ExecutionState) -> runbook_model::ExecutionView {
    runbook_model::ExecutionView {
        schema_revision: std::num::NonZeroU32::MIN,
        id: state.id.clone(),
        runbook_id: None,
        runbook_slug: None,
        project_id: None,
        created_at_ms: None,
        created_by: None,
        started_at_ms: state.started_at_ms,
        completed_at_ms: None,
        runbook_version_id: "rbv_00000000000000000000000001".parse().unwrap(),
        runbook_version: Some(1),
        title: state.title.clone(),
        status: state.status,
        phase: runbook_model::ExecutionPhase::Ordinary,
        admission: None,
        execution_deadline_at_ms: None,
        result: None,
        pending_terminal_status: None,
        terminal_reason: None,
        inputs: state.inputs.clone(),
        fields: state.fields.clone(),
        tags: state.tags.clone(),
        steps: vec![],
        amended: false,
        amendments: vec![],
        stale_step_ids: vec![],
        rerun_of: None,
        resume_brief: None,
        notifications_not_delivered: vec![],
        superseded_by: None,
        parent: None,
        progress: None,
        cost: None,
    }
}

/// LANG3 race fixture (Postgres runs it with one pool per contender): the
/// `cancel_previous` runbook published as runbook 4 and run 1 holding the
/// key.
pub async fn on_conflict_race_setup<S: LanguageStore, D: LanguageDirectory>(store: &S, dir: &D) {
    directory(dir).await;
    crate::parity::publish(
        store,
        4,
        "latest",
        &runbook_source("latest", 1, "cancel_previous", true),
        4,
    )
    .await;
    start_with_policy(store, dir, 1, 4).await.unwrap();
}
/// End run 1's cleanup in the race fixture.
pub async fn on_conflict_race_finish_cleanup<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    report(store, dir, 1, "cleanup", 30, None, serde_json::json!({})).await;
}
/// Start run `n` of the race fixture's `cancel_previous` runbook.
pub async fn on_conflict_race_start<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    n: u8,
) -> Result<runbook_core::start::StartOutcome, runbook_core::start::StartError> {
    start_with_policy(store, dir, n, 4).await
}

/// LANG3 `reject` race fixture: the `reject` runbook published as runbook
/// 3. With `warm`, a first run has started and finished, so the key's pool
/// exists and is idle.
pub async fn on_conflict_reject_race_setup<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    warm: bool,
) {
    directory(dir).await;
    crate::parity::publish(
        store,
        3,
        "guarded",
        &runbook_source("guarded", 1, "reject", false),
        3,
    )
    .await;
    if warm {
        start_with_policy(store, dir, 40, 3).await.unwrap();
        report(store, dir, 40, "work", 40, None, serde_json::json!({})).await;
    }
}
/// Start run `n` of the `reject` race fixture.
pub async fn on_conflict_reject_race_start<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    n: u8,
) -> Result<runbook_core::start::StartOutcome, runbook_core::start::StartError> {
    start_with_policy(store, dir, n, 3).await
}

const JOIN_TIMERS: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: timers, title: Timers}\n\
    spec:\n  defaults: {actor: agent}\n  steps:\n\
    \x20   - id: fan\n      join: any\n      parallel:\n\
    \x20       - {id: fast, title: Fast}\n\
    \x20       - {id: pause, wait: {for: 1h}}\n\
    \x20       - {id: gate, approval: {prompt: Go?, options: [go], approvers: {teams: [oncall]}, timeout: 1h, on_timeout: escalate, escalate_to: [team:oncall]}}\n";

/// LANG2: when a join is met, the cancelled branches' wait and approval
/// timers are deleted with them.
pub async fn parallel_join_clears_timers_of_cancelled_branches<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    crate::parity::publish(store, 3, "timers", JOIN_TIMERS, 3).await;
    crate::portable::start(store, dir, &p(), 1, 3).await;
    // The wait (1 h) and the approval timeout (1 h) are the run's next wakeup.
    let before = store.load(&exe(1)).await.unwrap().timers;
    assert!(!before.is_empty(), "{before:?}");
    report(store, dir, 1, "fast", 1, None, serde_json::json!({})).await;
    let state = run_state(store, dir, 1).await;
    for id in ["pause", "gate"] {
        let step = state.steps.iter().find(|s| s.step_id == id).unwrap();
        assert_eq!(step.status, StepStatus::Cancelled, "{id}");
    }
    let after = store.load(&exe(1)).await.unwrap().timers;
    assert!(after.is_empty(), "{after:?}");
    assert_eq!(
        crate::portable::status(store, 1).await.unwrap(),
        runbook_model::ExecutionStatus::Succeeded
    );
}

const INPUT_TYPES_SPEC: &str = r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: input-types
  title: Input Types
spec:
  defaults: {actor: agent}
  inputs:
    tags: {type: multi_enum, options: [web, api, worker, db]}
    deploy_date: {type: date}
    scheduled_at: {type: datetime}
    timeout: {type: duration}
    docs_url: {type: url}
    contact: {type: email}
    owner: {type: user}
    code: {type: string, pattern: "^[A-Z]{3}-\\d{3}$"}
  steps:
    - {id: work, title: Work}
  outputs:
    code: "${{ inputs.code }}"
    contact: "${{ inputs.contact }}"
"#;

fn valid_inputs() -> runbook_model::Data {
    [
        ("tags".into(), serde_json::json!(["web", "api"])),
        ("deploy_date".into(), serde_json::json!("2026-11-12")),
        (
            "scheduled_at".into(),
            serde_json::json!("2026-11-12T19:00:00Z"),
        ),
        ("timeout".into(), serde_json::json!("15m")),
        (
            "docs_url".into(),
            serde_json::json!("https://example.com/docs"),
        ),
        ("contact".into(), serde_json::json!("ops@example.com")),
        (
            "owner".into(),
            serde_json::json!("user:usr_00000000000000000000000001"),
        ),
        ("code".into(), serde_json::json!("ABC-123")),
    ]
    .into_iter()
    .collect()
}

fn start_command_with_inputs(
    n: u8,
    runbook: u8,
    inputs: runbook_model::Data,
) -> runbook_core::start::StartCommand {
    let mut cmd = start_command(n, runbook);
    cmd.inputs = inputs;
    cmd
}

async fn start_with_inputs<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    n: u8,
    runbook: u8,
    inputs: runbook_model::Data,
) -> Result<runbook_core::start::StartOutcome, runbook_core::start::StartError> {
    let commit = runbook_core::start::prepare_start_commit(
        store,
        store,
        &start_command_with_inputs(n, runbook, inputs),
    )
    .await?;
    let outcome = runbook_core::start::commit_start_with_policy(store, dir, &p(), commit).await?;
    activate_due(store, dir, &p()).await;
    Ok(outcome)
}

/// LANG5 store test: `multi_enum`, `date`, `datetime`, `duration`, `url`,
/// `email`, `user`, and `pattern` validation at execution start on every adapter;
/// invalid inputs are refused with `execution.invalid_data`, valid runs succeed,
/// and the committed log replays to the same outputs and inputs.
pub async fn input_types_and_pattern_run_and_replay<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    crate::parity::publish(store, 3, "input-types", INPUT_TYPES_SPEC, 3).await;
    let invalid_inputs = [
        ("tags", serde_json::json!(["web", "invalid"])),
        ("deploy_date", serde_json::json!("not-a-date")),
        ("scheduled_at", serde_json::json!("not-a-datetime")),
        ("timeout", serde_json::json!("invalid-duration")),
        ("docs_url", serde_json::json!("not a url")),
        ("contact", serde_json::json!("not-an-email")),
        ("owner", serde_json::json!("not a user")),
        ("code", serde_json::json!("invalid-code")),
    ];
    for (idx, (field, val)) in invalid_inputs.into_iter().enumerate() {
        let mut inputs = valid_inputs();
        inputs.insert(field.into(), val);
        let res = start_with_inputs(store, dir, 50 + idx as u8, 3, inputs).await;
        assert!(
            matches!(res, Err(runbook_core::start::StartError::InvalidData(_))),
            "field {field} should fail with InvalidData"
        );
    }

    let valid = valid_inputs();
    start_with_inputs(store, dir, 1, 3, valid).await.unwrap();
    report(store, dir, 1, "work", 1, None, serde_json::json!({})).await;
    assert_eq!(
        crate::portable::status(store, 1).await.unwrap(),
        runbook_model::ExecutionStatus::Succeeded
    );
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let state = work.runtime.state;
    assert_eq!(state.status, runbook_model::ExecutionStatus::Succeeded);
    assert_eq!(state.outputs["code"], serde_json::json!("ABC-123"));
    assert_eq!(
        state.outputs["contact"],
        serde_json::json!("ops@example.com")
    );
    assert_eq!(state.inputs["code"], serde_json::json!("ABC-123"));
    assert_eq!(state.inputs["timeout"], serde_json::json!("15m"));
    assert_eq!(state.inputs["tags"], serde_json::json!(["web", "api"]));
    assert_eq!(state.inputs["deploy_date"], serde_json::json!("2026-11-12"));
    assert_eq!(
        state.inputs["scheduled_at"],
        serde_json::json!("2026-11-12T19:00:00Z")
    );
    assert_eq!(
        state.inputs["docs_url"],
        serde_json::json!("https://example.com/docs")
    );
    assert_eq!(
        state.inputs["owner"],
        serde_json::json!("user:usr_00000000000000000000000001")
    );
}

// LANG4: `call`.

const CALL_CHILD: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: db-migrate, title: Migrate}\n\
    spec:\n  defaults: {actor: agent}\n\
    \x20 inputs:\n    target: {type: string, required: true}\n\
    \x20 steps:\n\
    \x20   - {id: migrate, title: \"Migrate ${{ inputs.target }}\", outputs: {rows: {type: integer, required: true}}}\n\
    \x20 outputs:\n    rows: \"${{ steps.migrate.outputs.rows }}\"\n";
const CALL_PARENT: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: deploy, title: Deploy}\n\
    spec:\n  defaults: {actor: agent}\n  steps:\n\
    \x20   - id: db\n      call:\n        runbook: db-migrate\n        version: 1\n\
    \x20       with: {target: prod}\n        outputs: {rows: {type: integer}}\n\
    \x20   - {id: verify, title: \"Verify ${{ steps.db.outputs.rows }} rows\"}\n";

/// Publish the child (`db-migrate`, runbook 4) and the parent (`deploy`,
/// runbook 3, calling the child's version 1) the way the authoring routes
/// do: the parent's compile context resolves its call from the catalog.
async fn publish_call_pair<S: LanguageStore>(store: &S) {
    crate::parity::publish(store, 4, "db-migrate", CALL_CHILD, 4).await;
    store
        .create_runbook(
            runbook_core::catalog::RunbookRecord {
                runbook_id: crate::portable::rb(3),
                slug: "deploy".into(),
                origin: runbook_core::catalog::RunbookOrigin::Ui,
                archived: false,
            },
            CALL_PARENT.into(),
        )
        .await
        .unwrap();
    let runbooks = [
        store
            .read_runbook(&crate::portable::rb(4))
            .await
            .unwrap()
            .unwrap(),
        store
            .read_runbook(&crate::portable::rb(3))
            .await
            .unwrap()
            .unwrap(),
    ];
    let context = runbook_spec::CompileContext {
        runbooks: runbook_core::calls::call_targets(store, &runbooks, CALL_PARENT)
            .await
            .unwrap(),
        ..Default::default()
    };
    assert_eq!(context.runbooks.len(), 1, "the call resolves");
    let prepared = runbook_core::publish::prepare(
        "deploy",
        CALL_PARENT,
        &context,
        "rbv_00000000000000000000000003".parse().unwrap(),
    )
    .unwrap();
    runbook_core::publish::publish_draft(store, store, &crate::portable::rb(3), 1, prepared)
        .await
        .unwrap();
}

/// Process everything due (bounded): activations, timers and call work.
/// Notification leases are left to the webhook drain.
pub async fn drain_calls<S: LanguageStore, D: LanguageDirectory>(store: &S, dir: &D) {
    for _ in 0..20 {
        let leases = store.claim_due(64, 60_000).await.unwrap();
        let work: Vec<_> = leases
            .into_iter()
            .filter(|l| {
                !matches!(
                    l.id.kind,
                    runbook_core::work_queue::WorkKind::Notification { .. }
                )
            })
            .collect();
        if work.is_empty() {
            return;
        }
        for lease in &work {
            runbook_core::drain::process_lease(store, dir, p(), lease)
                .await
                .unwrap_or_else(|e| panic!("{:?}: {e:?}", lease.id));
        }
    }
    panic!("call work did not settle");
}

async fn call_state<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    id: &runbook_model::ExecutionId,
) -> runbook_engine::ExecutionState {
    begin_actor(store, dir, p(), id)
        .await
        .unwrap()
        .runtime
        .state
}

/// Claim one agent task of run `id` and report it succeeded with `outputs`.
async fn report_on<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    id: &runbook_model::ExecutionId,
    step_id: &str,
    claim: u8,
    outputs: serde_json::Value,
) {
    let step = call_state(store, dir, id)
        .await
        .steps
        .iter()
        .find(|s| s.step_id == step_id)
        .and_then(|s| s.id.clone())
        .unwrap_or_else(|| panic!("{step_id} is not materialized"));
    let work = begin_actor(store, dir, p(), id).await.unwrap();
    let claim = claim_step(
        work,
        &agent(),
        &step,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        format!("clm_{claim:026}").parse().unwrap(),
        None,
    )
    .await
    .unwrap();
    let work = begin_actor(store, dir, p(), id).await.unwrap();
    report_step(
        work,
        &agent(),
        &ReportRequest {
            step_run_id: step,
            claim,
            status: ReportStatus::Succeeded,
            verdict: None,
            outputs: serde_json::from_value(outputs).unwrap(),
            summary: None,
            evidence: vec![],
            logs: vec![],
            cost: None,
        },
        None,
    )
    .await
    .unwrap();
}

/// The call facts of one run's log, for the pinned trace: every event's
/// sequence and type, and the cross-execution events' fields.
async fn call_trace<S: LanguageStore>(
    store: &S,
    id: &runbook_model::ExecutionId,
) -> serde_json::Value {
    let run = store.load(id).await.unwrap();
    serde_json::Value::Array(
        run.events
            .iter()
            .map(|e| {
                let mut entry = serde_json::json!({"seq": e.seq, "type": e.event.name()});
                match &e.event {
                    runbook_engine::Event::CallRequested { start } => {
                        entry["child_execution_id"] = serde_json::json!(start.child_execution_id);
                        entry["inputs"] = serde_json::json!(start.inputs);
                    }
                    runbook_engine::Event::CallStarted {
                        child_execution_id, ..
                    } => entry["child_execution_id"] = serde_json::json!(child_execution_id),
                    runbook_engine::Event::CallCompleted { outcome } => {
                        entry["status"] = serde_json::json!(outcome.status);
                        entry["outputs"] = serde_json::json!(outcome.outputs);
                        entry["error"] = serde_json::json!(outcome.error);
                    }
                    runbook_engine::Event::ExecutionStarted { data, .. } => {
                        entry["parent"] = serde_json::json!(data.context.parent);
                    }
                    runbook_engine::Event::ExecutionCompleted { status, .. } => {
                        entry["status"] = serde_json::json!(status);
                    }
                    _ => {}
                }
                entry
            })
            .collect(),
    )
}

/// LANG4 store test: a `call` step starts its child through the parent's
/// work item (one start, counted like any other), waits, and takes the
/// child's outcome and outputs; both runs link each other; the logs replay.
/// Returns the pinned conformance trace (`conformance/execution/call.json`).
pub async fn call_runs_a_child_and_takes_its_outcome<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) -> serde_json::Value {
    directory(dir).await;
    publish_call_pair(store).await;
    start(store, dir, &p(), 1, 3).await;
    let parent = call_state(store, dir, &exe(1)).await;
    let db = parent.steps.iter().find(|s| s.step_id == "db").unwrap();
    assert_eq!(db.status, StepStatus::Waiting);
    let step_run = db.id.clone().unwrap();
    let child = runbook_core::calls::child_execution_id(&exe(1), &step_run);
    // Nothing started the child yet: its start is the parent's work item.
    assert!(matches!(
        store.load(&child).await,
        Err(runbook_core::StoreError::NotFound)
    ));
    drain_calls(store, dir).await;
    let started = call_state(store, dir, &child).await;
    assert_eq!(started.status, runbook_model::ExecutionStatus::Running);
    let link = started.parent.clone().expect("the child names its parent");
    assert_eq!(
        (link.execution_id.clone(), link.step_run_id.clone()),
        (exe(1), step_run.clone())
    );
    assert_eq!(link.depth, 1);
    assert_eq!(started.inputs["target"], serde_json::json!("prod"));
    let record = store.start_record(&child).await.unwrap().unwrap();
    assert_eq!(record.runbook_id, crate::portable::rb(4));
    assert!(
        call_state(store, dir, &exe(1))
            .await
            .steps
            .iter()
            .find(|s| s.step_id == "db")
            .unwrap()
            .call
            .as_ref()
            .unwrap()
            .started
    );
    // A second drain starts nothing more.
    drain_calls(store, dir).await;
    assert_eq!(
        store.load(&child).await.unwrap().events.len(),
        started.sequence as usize
    );

    report_on(
        store,
        dir,
        &child,
        "migrate",
        1,
        serde_json::json!({"rows": 7}),
    )
    .await;
    drain_calls(store, dir).await;
    let child_done = call_state(store, dir, &child).await;
    assert_eq!(child_done.status, runbook_model::ExecutionStatus::Succeeded);
    let parent = call_state(store, dir, &exe(1)).await;
    let db = parent.steps.iter().find(|s| s.step_id == "db").unwrap();
    assert_eq!(db.status, StepStatus::Succeeded);
    assert_eq!(db.outputs["rows"], serde_json::json!(7));
    report_on(store, dir, &exe(1), "verify", 2, serde_json::json!({})).await;
    drain_calls(store, dir).await;
    assert_eq!(
        call_state(store, dir, &exe(1)).await.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    // The views link both ways.
    let parent = call_state(store, dir, &exe(1)).await;
    let link = runbook_core::execution_document::call_of(
        parent.steps.iter().find(|s| s.step_id == "db").unwrap(),
    )
    .unwrap();
    assert_eq!(link.child_execution_id, child);
    assert!(link.started && link.error.is_none());
    let back = runbook_core::execution_document::parent_of(&child_done).unwrap();
    assert_eq!((back.execution_id, back.step.as_str()), (exe(1), "db"));
    serde_json::json!({
        "parent": call_trace(store, &exe(1)).await,
        "child": call_trace(store, &child).await,
    })
}

/// LANG4 store test: cancelling the parent while its child runs cancels
/// the child; the child's late result is not applied to the closed parent.
pub async fn cancelling_a_parent_cancels_its_child<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    publish_call_pair(store).await;
    start(store, dir, &p(), 1, 3).await;
    drain_calls(store, dir).await;
    let step_run = call_state(store, dir, &exe(1))
        .await
        .steps
        .iter()
        .find(|s| s.step_id == "db")
        .and_then(|s| s.id.clone())
        .unwrap();
    let child = runbook_core::calls::child_execution_id(&exe(1), &step_run);
    assert_eq!(
        call_state(store, dir, &child).await.status,
        runbook_model::ExecutionStatus::Running
    );
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    runbook_core::control::cancel(store, work, &crate::portable::human(1), None)
        .await
        .unwrap();
    drain_calls(store, dir).await;
    assert_eq!(
        call_state(store, dir, &exe(1)).await.status,
        runbook_model::ExecutionStatus::Cancelled
    );
    let cancelled = call_state(store, dir, &child).await;
    assert_eq!(cancelled.status, runbook_model::ExecutionStatus::Cancelled);
    // The parent's log ends with its own cancellation, not the child's.
    let parent_log = store.load(&exe(1)).await.unwrap().events;
    assert!(!parent_log.iter().any(|e| e.event.name() == "CallCompleted"));
}

/// LANG4 store test (children count toward the run quota): with one run
/// left this month, the parent starts and its child is refused for quota;
/// the call step fails with `limit.executions_monthly` and nothing of the
/// child is written. `limited` is `store` with a monthly limit of 1.
pub async fn a_child_refused_for_quota_fails_the_call_step<S, D>(store: &S, limited: &S, dir: &D)
where
    S: LanguageStore + runbook_core::quotas::QuotaStore,
    D: LanguageDirectory,
{
    directory(dir).await;
    publish_call_pair(store).await;
    start(limited, dir, &p(), 1, 3).await;
    drain_calls(limited, dir).await;
    let parent = call_state(limited, dir, &exe(1)).await;
    let db = parent.steps.iter().find(|s| s.step_id == "db").unwrap();
    assert_eq!(db.status, StepStatus::Failed);
    let call = db.call.as_ref().unwrap();
    assert_eq!(
        call.error.as_deref(),
        Some(runbook_core::quotas::MONTHLY_CODE)
    );
    assert_eq!(parent.status, runbook_model::ExecutionStatus::Failed);
    assert!(matches!(
        limited.load(&call.child_execution_id).await,
        Err(runbook_core::StoreError::NotFound)
    ));
    let usage = limited.org_usage(&p().org_id).await.unwrap();
    assert_eq!(usage.executions_started, 1, "only the parent counted");
}

// LANG4 review: crashes, races, nesting and fan-out.

/// Publish runbook `n` (`slug`, version ID `rbv_…n`) with its calls
/// resolved from the catalog, as the authoring routes do.
async fn publish_calling<S: LanguageStore>(store: &S, n: u8, slug: &str, source: &str) {
    store
        .create_runbook(
            runbook_core::catalog::RunbookRecord {
                runbook_id: crate::portable::rb(n),
                slug: slug.into(),
                origin: runbook_core::catalog::RunbookOrigin::Ui,
                archived: false,
            },
            source.into(),
        )
        .await
        .unwrap();
    let mut runbooks = vec![];
    for k in 1..=20u8 {
        if let Ok(Some(r)) = store.read_runbook(&crate::portable::rb(k)).await {
            runbooks.push(r);
        }
    }
    let context = runbook_spec::CompileContext {
        runbooks: runbook_core::calls::call_targets(store, &runbooks, source)
            .await
            .unwrap(),
        ..Default::default()
    };
    let prepared = runbook_core::publish::prepare(
        slug,
        source,
        &context,
        format!("rbv_{n:026}").parse().unwrap(),
    )
    .unwrap_or_else(|d| panic!("{slug}: {d:?}"));
    runbook_core::publish::publish_draft(store, store, &crate::portable::rb(n), 1, prepared)
        .await
        .unwrap();
}

/// Process due work except `skip`'s, which is returned still leased (a
/// worker that has not got to it yet).
async fn drain_except<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    skip: &runbook_model::ExecutionId,
) -> Vec<runbook_core::work_queue::WorkLease> {
    let mut held = vec![];
    for _ in 0..20 {
        let leases = store.claim_due(64, 60_000).await.unwrap();
        let mut worked = false;
        for lease in leases {
            if matches!(
                lease.id.kind,
                runbook_core::work_queue::WorkKind::Notification { .. }
            ) {
                continue;
            }
            if &lease.id.execution_id == skip {
                held.push(lease);
                continue;
            }
            worked = true;
            runbook_core::drain::process_lease(store, dir, p(), &lease)
                .await
                .unwrap_or_else(|e| panic!("{:?}: {e:?}", lease.id));
        }
        if !worked {
            return held;
        }
    }
    panic!("work did not settle");
}

/// The call step `step_id` of run `id`: its step run and its child.
async fn call_of<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    id: &runbook_model::ExecutionId,
    step_id: &str,
) -> (StepRunId, runbook_model::ExecutionId) {
    let state = call_state(store, dir, id).await;
    let step = state.steps.iter().find(|s| s.step_id == step_id).unwrap();
    let sr = step.id.clone().unwrap();
    (sr.clone(), runbook_core::calls::child_execution_id(id, &sr))
}

fn names(run: &runbook_core::StoredExecution) -> Vec<&'static str> {
    run.events.iter().map(|e| e.event.name()).collect()
}

/// Review 1: worker A's lease lapsed while it started the child; worker B
/// replayed the start and recorded it. When A finally records, it finds the
/// start already there and leaves the (wanted) child running.
pub async fn overlapping_start_leases_never_cancel_a_wanted_child<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    publish_call_pair(store).await;
    start(store, dir, &p(), 1, 3).await;
    // Worker A read the parent while the step awaited its child's start.
    let seen_by_a = call_state(store, dir, &exe(1)).await;
    let (sr, child) = call_of(store, dir, &exe(1), "db").await;
    // Worker B does the whole item.
    drain_calls(store, dir).await;
    assert!(
        call_state(store, dir, &exe(1))
            .await
            .steps
            .iter()
            .find(|s| s.step_id == "db")
            .unwrap()
            .call
            .as_ref()
            .unwrap()
            .started
    );
    // A resumes from what it read.
    let done = runbook_core::calls::start_child(store, dir, &p(), &seen_by_a, &sr)
        .await
        .unwrap();
    assert!(done);
    drain_calls(store, dir).await;
    assert_eq!(
        call_state(store, dir, &child).await.status,
        runbook_model::ExecutionStatus::Running
    );
    assert_eq!(
        names(&store.load(&child).await.unwrap())[0],
        "ExecutionStarted"
    );
    // Exactly one child, which runs to the end and completes the step.
    report_on(
        store,
        dir,
        &child,
        "migrate",
        1,
        serde_json::json!({"rows": 3}),
    )
    .await;
    drain_calls(store, dir).await;
    let parent = call_state(store, dir, &exe(1)).await;
    let db = parent.steps.iter().find(|s| s.step_id == "db").unwrap();
    assert_eq!(db.status, StepStatus::Succeeded);
}

/// Review 8: a crash after the child's start committed and before its
/// parent recorded it: the retried item finds the child and records it,
/// starting nothing twice.
pub async fn a_crash_between_the_child_and_its_start_record_recovers<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    publish_call_pair(store).await;
    start(store, dir, &p(), 1, 3).await;
    let (sr, child) = call_of(store, dir, &exe(1), "db").await;
    let parent = call_state(store, dir, &exe(1)).await;
    assert_eq!(
        runbook_core::calls::commit_child_start(store, dir, &p(), &parent, &sr)
            .await
            .unwrap(),
        runbook_core::calls::ChildStart::Started
    );
    // The crash: nothing was recorded on the parent.
    assert!(
        !call_state(store, dir, &exe(1))
            .await
            .steps
            .iter()
            .find(|s| s.step_id == "db")
            .unwrap()
            .call
            .as_ref()
            .unwrap()
            .started
    );
    drain_calls(store, dir).await;
    let parent = call_state(store, dir, &exe(1)).await;
    let call = parent
        .steps
        .iter()
        .find(|s| s.step_id == "db")
        .unwrap()
        .call
        .clone()
        .unwrap();
    assert!(call.started && call.child_execution_id == child);
    let log = store.load(&child).await.unwrap();
    assert_eq!(
        names(&log)
            .iter()
            .filter(|n| **n == "ExecutionStarted")
            .count(),
        1
    );
    assert_eq!(
        names(&store.load(&exe(1)).await.unwrap())
            .iter()
            .filter(|n| **n == "CallStarted")
            .count(),
        1
    );
}

/// Review 8: the child's result reaches the parent before its start record
/// (a crash between the two); the step takes it, the late start record is
/// not applied and the finished child is not cancelled.
pub async fn a_result_before_the_start_record_is_kept<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    publish_call_pair(store).await;
    start(store, dir, &p(), 1, 3).await;
    let (sr, child) = call_of(store, dir, &exe(1), "db").await;
    let parent = call_state(store, dir, &exe(1)).await;
    runbook_core::calls::commit_child_start(store, dir, &p(), &parent, &sr)
        .await
        .unwrap();
    // The child runs and finishes while the parent's start item waits.
    let mut held = drain_except(store, dir, &exe(1)).await;
    report_on(
        store,
        dir,
        &child,
        "migrate",
        1,
        serde_json::json!({"rows": 9}),
    )
    .await;
    held.extend(drain_except(store, dir, &exe(1)).await);
    let parent = call_state(store, dir, &exe(1)).await;
    let db = parent.steps.iter().find(|s| s.step_id == "db").unwrap();
    assert_eq!(db.status, StepStatus::Succeeded);
    assert_eq!(db.outputs["rows"], serde_json::json!(9));
    assert!(!db.call.as_ref().unwrap().started);
    // The parent's start item, run late, changes nothing.
    for lease in &held {
        runbook_core::drain::process_lease(store, dir, p(), lease)
            .await
            .unwrap();
    }
    drain_calls(store, dir).await;
    assert_eq!(
        call_state(store, dir, &child).await.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert!(!names(&store.load(&exe(1)).await.unwrap()).contains(&"CallStarted"));
}

/// Review 2: the parent is cancelled while a worker is starting its child;
/// the cancel item finds no child and is done; then the child's start
/// commits and the worker fails before telling the parent. The child, once
/// activated, sees that nobody awaits it and is cancelled before it starts.
pub async fn a_child_started_after_its_parent_was_cancelled_never_runs<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    publish_call_pair(store).await;
    start(store, dir, &p(), 1, 3).await;
    let (sr, child) = call_of(store, dir, &exe(1), "db").await;
    let seen = call_state(store, dir, &exe(1)).await;
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    runbook_core::control::cancel(store, work, &crate::portable::human(1), None)
        .await
        .unwrap();
    drain_calls(store, dir).await;
    assert!(matches!(
        store.load(&child).await,
        Err(runbook_core::StoreError::NotFound)
    ));
    // The late start, from what the worker read before the cancellation.
    runbook_core::calls::commit_child_start(store, dir, &p(), &seen, &sr)
        .await
        .unwrap();
    drain_calls(store, dir).await;
    let run = store.load(&child).await.unwrap();
    assert_eq!(names(&run), ["ExecutionCancelledBeforeStart"]);
    assert_eq!(
        run.projection.status,
        runbook_model::ExecutionStatus::Cancelled
    );
}

const CALL_DEPTH_LEAF: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: leaf, title: Leaf}\n\
    spec:\n  defaults: {actor: agent}\n  steps: [{id: work, title: Work}]\n";

fn call_one(slug: &str, callee: &str) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: {slug}, title: {slug}}}\n\
         spec:\n  steps:\n    - {{id: next, call: {{runbook: {callee}}}}}\n"
    )
}

/// Review 8: cancelling the top of a three-deep chain cancels every run in
/// it.
pub async fn cancelling_the_top_of_a_call_chain_cancels_every_level<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    publish_calling(store, 4, "leaf", CALL_DEPTH_LEAF).await;
    publish_calling(store, 5, "mid", &call_one("mid", "leaf")).await;
    publish_calling(store, 6, "upper", &call_one("upper", "mid")).await;
    publish_calling(store, 3, "top", &call_one("top", "upper")).await;
    start(store, dir, &p(), 1, 3).await;
    drain_calls(store, dir).await;
    let mut chain = vec![exe(1)];
    for depth in 1..=3u32 {
        let (_, child) = call_of(store, dir, chain.last().unwrap(), "next").await;
        let state = call_state(store, dir, &child).await;
        assert_eq!(state.status, runbook_model::ExecutionStatus::Running);
        assert_eq!(state.parent.as_ref().unwrap().depth, depth);
        chain.push(child);
    }
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    runbook_core::control::cancel(store, work, &crate::portable::human(1), None)
        .await
        .unwrap();
    drain_calls(store, dir).await;
    for id in &chain {
        assert_eq!(
            store.load(id).await.unwrap().projection.status,
            runbook_model::ExecutionStatus::Cancelled,
            "{id}"
        );
    }
}

const CALL_FAN_OUT: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: fan-out, title: Fan out}\n\
    spec:\n  inputs:\n    targets: {type: list, items: {type: string}, default: [a, b, c]}\n\
    \x20 steps:\n\
    \x20   - id: each\n      foreach:\n        items: \"${{ inputs.targets }}\"\n        max_items: 3\n        concurrency: 3\n\
    \x20       steps:\n          - {id: db, call: {runbook: db-migrate, with: {target: \"${{ item }}\"}, outputs: {rows: {type: integer}}}}\n\
    \x20   - id: again\n      loop:\n        until: \"${{ false }}\"\n        max_iterations: 2\n\
    \x20       steps:\n          - {id: redo, call: {runbook: db-migrate, with: {target: loop}}}\n\
    \x20   - id: both\n      parallel:\n        - {id: left, call: {runbook: db-migrate, with: {target: left}}}\n\
    \x20       - {id: right, call: {runbook: db-migrate, with: {target: right}}}\n";

/// Review 8: calls in a foreach, a loop and a parallel join: one child per
/// call step run, each started once and finished into its own step.
pub async fn fan_out_calls_start_one_child_per_step_run<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    crate::parity::publish(store, 4, "db-migrate", CALL_CHILD, 4).await;
    publish_calling(store, 3, "fan-out", CALL_FAN_OUT).await;
    start(store, dir, &p(), 1, 3).await;
    let mut seen = std::collections::BTreeSet::new();
    let mut claim = 1u8;
    for _ in 0..10 {
        drain_calls(store, dir).await;
        let parent = call_state(store, dir, &exe(1)).await;
        if parent.status != runbook_model::ExecutionStatus::Running {
            break;
        }
        let waiting: Vec<_> = parent
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Waiting)
            .filter_map(|s| s.call.as_ref().map(|c| c.child_execution_id.clone()))
            .collect();
        assert!(
            !waiting.is_empty(),
            "the run is stuck: {:?}",
            names(&store.load(&exe(1)).await.unwrap())
        );
        for child in waiting {
            if seen.insert(child.clone()) {
                report_on(
                    store,
                    dir,
                    &child,
                    "migrate",
                    claim,
                    serde_json::json!({"rows": 1}),
                )
                .await;
                claim += 1;
            }
        }
    }
    let parent = call_state(store, dir, &exe(1)).await;
    assert_eq!(parent.status, runbook_model::ExecutionStatus::Succeeded);
    // Three foreach items, two loop iterations, two parallel branches.
    assert_eq!(seen.len(), 7);
    let calls: Vec<_> = parent
        .steps
        .iter()
        .filter_map(|s| s.call.as_ref())
        .collect();
    assert_eq!(calls.len(), 7);
    assert!(calls.iter().all(|c| c.completed && c.error.is_none()));
    for child in &seen {
        let log = store.load(child).await.unwrap();
        assert_eq!(
            names(&log)
                .iter()
                .filter(|n| **n == "ExecutionStarted")
                .count(),
            1
        );
    }
}

/// Review 5: a finished child whose parent still awaits its result is not
/// deleted by retention or a deletion request until the result is
/// delivered.
pub async fn a_child_is_not_deleted_before_its_result_is_delivered<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    publish_call_pair(store).await;
    start(store, dir, &p(), 1, 3).await;
    drain_calls(store, dir).await;
    let (_, child) = call_of(store, dir, &exe(1), "db").await;
    report_on(
        store,
        dir,
        &child,
        "migrate",
        1,
        serde_json::json!({"rows": 2}),
    )
    .await;
    // The child finished; its result item has not run yet.
    assert!(
        runbook_core::calls::stored_state(store, &exe(1))
            .await
            .unwrap()
            .is_some_and(|p| runbook_engine::awaits_outcome(&p, &call_of_sync(&p), &child))
    );
    assert!(
        runbook_core::calls::result_pending(store, &child)
            .await
            .unwrap()
    );
    drain_calls(store, dir).await;
    assert!(
        !runbook_core::calls::result_pending(store, &child)
            .await
            .unwrap()
    );
}

fn call_of_sync(parent: &runbook_engine::ExecutionState) -> StepRunId {
    parent
        .steps
        .iter()
        .find(|s| s.step_id == "db")
        .and_then(|s| s.id.clone())
        .unwrap()
}

/// Review 6: a call item, once done, is not listed as work again (natively
/// Postgres does not even read it): acknowledging it again finds no work.
pub async fn finished_call_items_are_no_longer_listed<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    directory(dir).await;
    publish_call_pair(store).await;
    start(store, dir, &p(), 1, 3).await;
    drain_calls(store, dir).await;
    let (_, child) = call_of(store, dir, &exe(1), "db").await;
    report_on(
        store,
        dir,
        &child,
        "migrate",
        1,
        serde_json::json!({"rows": 4}),
    )
    .await;
    let mut result = None;
    for lease in store.claim_due(64, 60_000).await.unwrap() {
        if matches!(
            lease.id.kind,
            runbook_core::work_queue::WorkKind::Notification { .. }
        ) {
            continue;
        }
        let is_result = matches!(&lease.id.kind,
            runbook_core::work_queue::WorkKind::Timer { key, .. } if key == runbook_core::calls::PARENT_KEY);
        runbook_core::drain::process_lease(store, dir, p(), &lease)
            .await
            .unwrap();
        if is_result {
            result = Some(lease);
        }
    }
    let result = result.expect("the child's result item was due");
    // The child's row stays, but the done item is not work any more.
    assert!(
        store
            .load(&child)
            .await
            .unwrap()
            .timers
            .contains_key(runbook_core::calls::PARENT_KEY)
    );
    assert_eq!(
        store.ack(&result.id, result.generation).await,
        Err(runbook_core::StoreError::InvalidBatch(
            "queue.work_obsolete"
        ))
    );
    assert!(store
        .claim_due(64, 60_000)
        .await
        .unwrap()
        .iter()
        .all(|l| l.id.execution_id != child
            || !matches!(&l.id.kind, runbook_core::work_queue::WorkKind::Timer { key, .. } if runbook_core::calls::is_call_key(key))));
}

/// Re-check 3: worker B decided its start of the child was refused (the
/// quota was full then); worker A's overlapping start of the same child
/// succeeded (the quota had room by then) and the child began running
/// while the step still awaited it. B then records the refusal, failing the
/// step, and A records its start: nobody awaits A's child any more, so A
/// cancels it rather than leave it running. `limited` is `store` with a
/// monthly limit of 1.
pub async fn a_child_started_after_its_refusal_was_recorded_is_cancelled<S, D>(
    store: &S,
    limited: &S,
    dir: &D,
) where
    S: LanguageStore + runbook_core::quotas::QuotaStore,
    D: LanguageDirectory,
{
    use runbook_core::calls::{ChildStart, commit_child_start, record_child_start};
    directory(dir).await;
    publish_call_pair(store).await;
    start(store, dir, &p(), 1, 3).await;
    let seen = call_state(store, dir, &exe(1)).await;
    let (sr, child) = call_of(store, dir, &exe(1), "db").await;
    // B decides: refused.
    let b = commit_child_start(limited, dir, &p(), &seen, &sr)
        .await
        .unwrap();
    assert_eq!(
        b,
        ChildStart::Refused(runbook_core::quotas::MONTHLY_CODE.into())
    );
    // A commits the child, which is activated while the step awaits it.
    let a = commit_child_start(store, dir, &p(), &seen, &sr)
        .await
        .unwrap();
    assert_eq!(a, ChildStart::Started);
    let held = drain_except(store, dir, &exe(1)).await;
    assert_eq!(
        call_state(store, dir, &child).await.status,
        runbook_model::ExecutionStatus::Running
    );
    // B records the refusal; then A records its start.
    assert!(
        record_child_start(store, dir, &p(), &exe(1), &sr, b)
            .await
            .unwrap()
    );
    assert!(
        record_child_start(store, dir, &p(), &exe(1), &sr, a)
            .await
            .unwrap()
    );
    let run = store.load(&child).await.unwrap();
    assert_eq!(
        run.projection.status,
        runbook_model::ExecutionStatus::Cancelled
    );
    let parent = call_state(store, dir, &exe(1)).await;
    assert_eq!(parent.status, runbook_model::ExecutionStatus::Failed);
    for lease in &held {
        runbook_core::drain::process_lease(store, dir, p(), lease)
            .await
            .unwrap();
    }
    drain_calls(store, dir).await;
}

// LANG7: strict assignment and take-over. Both runbooks are written with
// only IDs (no apiVersion, kind or titles) to run the lean form end to end.

const STRICT: &str = "metadata: {id: strict-work}\n\
    spec:\n  inputs: {owner: {type: string, default: 'user:carol'}}\n  steps:\n\
    \x20   - id: all\n      parallel:\n\
    \x20       - {id: page, actor: human, assignee: team:oncall, assignment: strict}\n\
    \x20       - {id: note, actor: human, assignee: team:oncall}\n\
    \x20       - {id: nobody, actor: human, assignee: '${{ inputs.owner }}', assignment: strict}\n";

const TAKE_OVER: &str = "metadata: {id: take-over}\n\
    spec:\n  steps:\n\
    \x20   - id: all\n      parallel:\n\
    \x20       - {id: build, actor: agent}\n\
    \x20       - {id: free, actor: any}\n\
    \x20       - {id: guarded, actor: agent, assignee: team:oncall, assignment: strict}\n";

/// Claim `step_id` of run `n` as `who`; the rejection code on refusal.
async fn try_claim<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    who: &runbook_core::actor::ActorPrincipal,
    n: u8,
    step_id: &str,
    take_over: bool,
    claim: u8,
) -> Result<runbook_model::ClaimRef, &'static str> {
    let id = step_run(store, dir, n, step_id)
        .await
        .unwrap_or_else(|| panic!("{step_id} is not materialized"));
    let work = begin_actor(store, dir, p(), &exe(n)).await.unwrap();
    claim_step(
        work,
        who,
        &id,
        &ClaimRequest {
            take_over,
            session_id: format!("s{claim}"),
            actor: None,
        },
        format!("clm_{claim:026}").parse().unwrap(),
        None,
    )
    .await
    .map_err(|e| e.code())
}

async fn state_of<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
    n: u8,
) -> runbook_engine::ExecutionState {
    begin_actor(store, dir, p(), &exe(n))
        .await
        .unwrap()
        .runtime
        .state
}

/// LANG7 store test: a strict assignee limits which people may claim (a
/// member of the team with project run access; an admin outside it may
/// not), a guidance assignee does not, and a strict assignee that resolves
/// to nobody is a routing error for the project admins. Titles the
/// runbook leaves out are derived from the IDs.
pub async fn strict_assignment_limits_who_claims<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    use crate::portable::human;
    directory(dir).await;
    crate::parity::publish(store, 3, "strict-work", STRICT, 3).await;
    start(store, dir, &p(), 1, 3).await;
    let state = state_of(store, dir, 1).await;
    assert_eq!(state.compiled().ir().metadata.title, "Strict work");
    let page = state.steps.iter().find(|s| s.step_id == "page").unwrap();
    assert_eq!(
        runbook_engine::step_title(&state, page).as_deref(),
        Some("Page")
    );
    // dave is a project admin outside oncall; bob is in oncall without
    // access to this project.
    assert_eq!(
        try_claim(store, dir, &human(4), 1, "page", false, 1).await,
        Err("claim.not_assignee")
    );
    assert_eq!(
        try_claim(store, dir, &human(2), 1, "page", false, 2).await,
        Err("claim.not_assignee")
    );
    try_claim(store, dir, &human(1), 1, "page", false, 3)
        .await
        .unwrap();
    // A guidance assignee only routes: anyone with run access may claim.
    try_claim(store, dir, &human(4), 1, "note", false, 4)
        .await
        .unwrap();
    // carol exists but has no project role: nobody is eligible.
    let state = state_of(store, dir, 1).await;
    let nobody = state.steps.iter().find(|s| s.step_id == "nobody").unwrap();
    assert_eq!(
        nobody.routing_error.as_deref(),
        Some("handoff.no_recipient")
    );
    let resolved = nobody.strict_assignee.as_ref().unwrap();
    assert!(resolved.unresolved());
    assert_eq!(
        resolved.notification_recipients,
        [runbook_model::RecipientRef::User(crate::portable::user(4))]
    );
    assert!(nobody.blocked_notification_queued);
    for (who, claim) in [(1, 5), (4, 6)] {
        assert_eq!(
            try_claim(store, dir, &human(who), 1, "nobody", false, claim).await,
            Err("claim.not_assignee")
        );
    }
    // The run document shows each assignee and whether it is strict.
    let state = state_of(store, dir, 1).await;
    let document = runbook_core::execution_document::build_document(&state, header(&state), |s| {
        format!("/v1/step-runs/{s}/logs")
    });
    let assignee = |path: &str| {
        document
            .sections
            .iter()
            .find(|s| s.path == path)
            .and_then(|s| s.assignee.clone())
    };
    let shown = |to: &str, strict: bool, routing_error: Option<&str>, team: bool| {
        Some(runbook_model::StepAssigneeView {
            to: Some(to.into()),
            strict,
            routing_error: routing_error.map(Into::into),
            recipients: if team {
                vec![runbook_model::RecipientRef::Team(
                    crate::portable::team().to_string(),
                )]
            } else {
                vec![]
            },
        })
    };
    assert_eq!(assignee("page"), shown("team:oncall", true, None, true));
    assert_eq!(assignee("note"), shown("team:oncall", false, None, false));
    assert_eq!(
        assignee("nobody"),
        shown("user:carol", true, Some("handoff.no_recipient"), false)
    );
    let page = document.sections.iter().find(|s| s.path == "page").unwrap();
    assert_eq!(page.task_actor, Some(runbook_model::TaskActor::Human));
    assert_eq!(assignee("all"), None);
    // The Inbox routes a strict step to its resolved assignee only: the
    // unresolved one goes to nobody (the admins got the blocked notice).
    let claims = begin_actor(store, dir, p(), &exe(1))
        .await
        .unwrap()
        .snapshot
        .claims;
    let routed = runbook_core::runtime::assignments(&state, &claims, state.sequence, &[]);
    let nobody_id = state
        .steps
        .iter()
        .find(|s| s.step_id == "nobody")
        .and_then(|s| s.id.clone())
        .unwrap();
    assert!(routed.iter().all(|a| a.step_run_id != nobody_id));
}

/// LANG7 store test: a person takes over a ready agent task (never a live
/// agent claim; an admin releases it first), the take-over is recorded,
/// agents and other tasks cannot use it, and a strict assignee still
/// applies to it.
pub async fn a_person_takes_over_an_agent_task<S: LanguageStore, D: LanguageDirectory>(
    store: &S,
    dir: &D,
) {
    use crate::portable::human;
    directory(dir).await;
    crate::parity::publish(store, 3, "take-over", TAKE_OVER, 3).await;
    start(store, dir, &p(), 1, 3).await;
    let alice = human(1);
    assert_eq!(
        try_claim(store, dir, &alice, 1, "build", false, 1).await,
        Err("claim.actor_mismatch")
    );
    try_claim(store, dir, &agent(), 1, "build", false, 2)
        .await
        .unwrap();
    assert_eq!(
        try_claim(store, dir, &alice, 1, "build", true, 3).await,
        Err("claim.take_over_live_claim")
    );
    let build = step_run(store, dir, 1, "build").await.unwrap();
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    runbook_core::claims::admin_release(work, &human(4), &build, "agent stuck", None)
        .await
        .unwrap();
    assert_eq!(
        try_claim(store, dir, &agent(), 1, "build", true, 4).await,
        Err("claim.take_over_not_allowed")
    );
    assert_eq!(
        try_claim(store, dir, &alice, 1, "free", true, 5).await,
        Err("claim.take_over_not_allowed")
    );
    let claim = try_claim(store, dir, &alice, 1, "build", true, 6)
        .await
        .unwrap();
    let state = state_of(store, dir, 1).await;
    let step = state.steps.iter().find(|s| s.step_id == "build").unwrap();
    assert_eq!(step.status, StepStatus::Claimed);
    let taken = step.taken_over.as_ref().unwrap();
    assert_eq!(taken.by, alice.actor);
    assert_eq!(taken.previous_actor_kind, runbook_model::ActorKind::Agent);
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    report_step(
        work,
        &alice,
        &ReportRequest {
            step_run_id: build,
            claim,
            status: ReportStatus::Succeeded,
            verdict: None,
            outputs: Default::default(),
            summary: Some("finished by hand".into()),
            evidence: vec![],
            logs: vec![],
            cost: None,
        },
        None,
    )
    .await
    .unwrap();
    let state = state_of(store, dir, 1).await;
    let step = state.steps.iter().find(|s| s.step_id == "build").unwrap();
    assert_eq!(step.status, StepStatus::Succeeded);
    assert_eq!(step.taken_over.as_ref().unwrap().by, alice.actor);
    let document = runbook_core::execution_document::build_document(&state, header(&state), |s| {
        format!("/v1/step-runs/{s}/logs")
    });
    let section = document
        .sections
        .iter()
        .find(|s| s.path == "build")
        .unwrap();
    assert_eq!(section.title, "Build");
    assert_eq!(section.actor.as_ref(), Some(&alice.actor));
    let taken = section.taken_over.as_ref().unwrap();
    assert_eq!(taken.by, alice.actor);
    assert_eq!(taken.previous_actor_kind, runbook_model::ActorKind::Agent);
    // A strict assignee binds the person taking over.
    assert_eq!(
        try_claim(store, dir, &human(4), 1, "guarded", true, 7).await,
        Err("claim.not_assignee")
    );
    try_claim(store, dir, &alice, 1, "guarded", true, 8)
        .await
        .unwrap();
}

// LANG7 review round 1.

/// carol (user 3) exists with no role in the project, so an assignee
/// naming her resolves to nobody until she is granted one.
const STRICT_ANY: &str = "metadata: {id: strict-any}\n\
    spec:\n  inputs: {owner: {type: string, default: 'user:usr_00000000000000000000000002'}}\n  steps:\n\
    \x20   - id: all\n      parallel:\n\
    \x20       - {id: page, actor: any, assignee: team:oncall, assignment: strict}\n\
    \x20       - {id: later, actor: any, assignee: '${{ inputs.owner }}', assignment: strict}\n\
    \x20       - {id: build, actor: agent, assignee: team:oncall, assignment: strict}\n\
    \x20       - {id: named, actor: any, assignee: user:alice, assignment: strict}\n";

/// LANG7 store test (review round 1): only the assignee, a person, may
/// claim a strict `actor: any` task; an agent is refused even when the
/// assignee resolved to nobody, and is not offered the work. On a strict
/// `actor: agent` task `actor` still governs agents.
pub async fn agents_may_not_claim_strict_work_open_to_anyone<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) {
    use crate::portable::human;
    directory(dir).await;
    crate::parity::publish(store, 3, "strict-any", STRICT_ANY, 3).await;
    start(store, dir, &p(), 1, 3).await;
    let state = state_of(store, dir, 1).await;
    let offered = |audience| {
        runbook_core::reads::resume_brief(&state, audience)
            .ready_work
            .into_iter()
            .map(|w| (w.path, w.actor_kind))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        offered(Some(runbook_model::ActorKind::Agent)),
        [("build".to_string(), runbook_model::ActorKind::Agent)]
    );
    assert_eq!(
        runbook_engine::reported_status(&state),
        runbook_model::ExecutionStatus::Running
    );
    for (step, claim) in [("page", 1), ("later", 2)] {
        assert_eq!(
            try_claim(store, dir, &agent(), 1, step, false, claim).await,
            Err("claim.not_assignee"),
            "{step}"
        );
    }
    // dave is a project admin outside oncall.
    assert_eq!(
        try_claim(store, dir, &human(4), 1, "page", false, 3).await,
        Err("claim.not_assignee")
    );
    try_claim(store, dir, &human(1), 1, "page", false, 4)
        .await
        .unwrap();
    try_claim(store, dir, &agent(), 1, "build", false, 5)
        .await
        .unwrap();
    // A user assignee written by name resolves through the runbook's
    // bindings to that user.
    let state = state_of(store, dir, 1).await;
    let named = state.steps.iter().find(|s| s.step_id == "named").unwrap();
    assert_eq!(named.routing_error, None);
    assert_eq!(
        named.strict_assignee.as_ref().unwrap().eligible_recipients,
        [runbook_model::RecipientRef::User(crate::portable::user(1))]
    );
    for (who, claim) in [(agent(), 6), (human(4), 7)] {
        assert_eq!(
            try_claim(store, dir, &who, 1, "named", false, claim).await,
            Err("claim.not_assignee")
        );
    }
    try_claim(store, dir, &human(1), 1, "named", false, 8)
        .await
        .unwrap();
}

/// LANG7 store test (review round 1): a strict assignee that resolved to
/// nobody is resolved again against the live directory when someone
/// claims. Once the named person is granted a role, that person may claim
/// (the new resolution is recorded and the Inbox routes to them); nobody
/// else may, before or after.
pub async fn an_unresolved_strict_assignee_is_resolved_again_at_claim<
    S: LanguageStore,
    D: LanguageDirectory,
>(
    store: &S,
    dir: &D,
) {
    use crate::portable::{human, user};
    use runbook_core::directory::{DirectoryCommit, DirectoryMutation};
    directory(dir).await;
    crate::parity::publish(store, 3, "strict-any", STRICT_ANY, 3).await;
    start(store, dir, &p(), 1, 3).await;
    let later = |state: &runbook_engine::ExecutionState| {
        state
            .steps
            .iter()
            .find(|s| s.step_id == "later")
            .unwrap()
            .clone()
    };
    let before = later(&state_of(store, dir, 1).await);
    assert!(before.strict_assignee.as_ref().unwrap().unresolved());
    assert_eq!(
        before.routing_error.as_deref(),
        Some("handoff.no_recipient")
    );
    // Still nobody: bob has no role yet.
    assert_eq!(
        try_claim(store, dir, &human(2), 1, "later", false, 1).await,
        Err("claim.not_assignee")
    );
    let revision = dir.snapshot(&p().org_id).await.unwrap().revision;
    dir.commit(DirectoryCommit {
        org_id: p().org_id,
        expected_revision: revision,
        mutations: vec![DirectoryMutation::GrantProjectRole {
            project_id: p().project_id,
            user_id: user(2),
            role: runbook_model::ProjectRole::Operator,
        }],
    })
    .await
    .unwrap();
    // Others stay refused, and a refusal records nothing.
    for (who, claim) in [(human(4), 2), (human(1), 3), (agent(), 4)] {
        assert_eq!(
            try_claim(store, dir, &who, 1, "later", false, claim).await,
            Err("claim.not_assignee")
        );
    }
    assert_eq!(
        later(&state_of(store, dir, 1).await).strict_assignee,
        before.strict_assignee
    );
    try_claim(store, dir, &human(2), 1, "later", false, 5)
        .await
        .unwrap();
    let state = state_of(store, dir, 1).await;
    let step = later(&state);
    assert_eq!(step.status, StepStatus::Claimed);
    assert_eq!(step.routing_error, None);
    let resolved = step.strict_assignee.as_ref().unwrap();
    assert_eq!(
        resolved.eligible_recipients,
        [runbook_model::RecipientRef::User(user(2))]
    );
    assert!(resolved.notification_recipients.is_empty());
    assert!(resolved.directory_revision > revision);
    let claims = begin_actor(store, dir, p(), &exe(1))
        .await
        .unwrap()
        .snapshot
        .claims;
    let routed = runbook_core::runtime::assignments(&state, &claims, state.sequence, &[]);
    let id = step.id.clone().unwrap();
    assert_eq!(
        routed
            .iter()
            .filter(|a| a.step_run_id == id)
            .map(|a| a.recipients.clone())
            .collect::<Vec<_>>(),
        [vec![runbook_model::RecipientRef::User(user(2))]]
    );
}
