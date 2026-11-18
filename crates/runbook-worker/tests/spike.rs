//! CF1: the spike's portable logic, natively, against the core's in-memory
//! `ExecutionStore` fake. `cloudflare/tests` runs the same flow in the
//! ExecutionDO under workerd.
use runbook_core::{
    CommitResult, ExecutionStore, StoreError, TimerChange, WriteStage, start::project,
    store::memory::MemoryExecutionStore,
};
use runbook_engine::ExecutionState;
use runbook_model::{ExecutionId, ExecutionStatus, StepStatus};
use runbook_worker::spike::{self, RUNBOOK_ID, VERSION_ID};
use std::rc::Rc;

const T0: i64 = 1_700_000_000_000;

fn execution_id() -> ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}

fn store() -> MemoryExecutionStore {
    let state = ExecutionState::new(execution_id(), Rc::new(spike::compiled().unwrap()));
    let projection = project(
        &state,
        RUNBOOK_ID.parse().unwrap(),
        VERSION_ID.parse().unwrap(),
    );
    MemoryExecutionStore::new(spike::scope(), projection, T0)
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    // The store futures never pend: poll once with a no-op waker.
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("store future pended"),
    }
}

fn status(run: &runbook_core::StoredExecution, path: &str) -> StepStatus {
    run.projection
        .steps
        .iter()
        .find(|step| step.path == path)
        .map(|step| step.status)
        .unwrap_or_else(|| panic!("no step {path}"))
}

#[test]
fn start_commit_carries_the_first_events_and_the_wait_timer() {
    let commit = spike::start_commit(&execution_id(), T0).unwrap();
    assert_eq!(commit.validate(), Ok(commit.events.len() as u64));
    assert_eq!(spike::event_type(&commit.events[0]), "ExecutionStarted");
    assert!(commit.events.iter().all(|e| e.at_ms == T0));
    let due: Vec<i64> = commit
        .timers
        .iter()
        .filter_map(|change| match change {
            TimerChange::Upsert(timer) => Some(timer.due_at_ms),
            TimerChange::Delete { .. } => None,
        })
        .collect();
    assert_eq!(due, [T0 + 1_000]);
    assert_eq!(commit.derived_outbox.len(), 1);
}

#[test]
fn the_alarm_advance_fires_the_wait_and_readies_the_next_step() {
    let store = store();
    let id = execution_id();
    assert!(matches!(
        block_on(spike::start_with(&store, &id, T0)).unwrap(),
        CommitResult::Committed(_)
    ));
    let started = block_on(store.load(&id)).unwrap();
    assert_eq!(status(&started, "pause"), StepStatus::Waiting);
    // An early alarm finds nothing due.
    assert_eq!(spike::advance_commit(&started, T0 + 999).unwrap(), None);
    let commit = spike::advance_commit(&started, T0 + 1_000)
        .unwrap()
        .expect("the wait is due");
    assert_eq!(commit.expected_sequence, started.revision);
    assert!(matches!(
        block_on(store.commit(commit)).unwrap(),
        CommitResult::Committed(_)
    ));
    let advanced = block_on(store.load(&id)).unwrap();
    assert_eq!(status(&advanced, "pause"), StepStatus::Succeeded);
    assert_eq!(status(&advanced, "next"), StepStatus::Ready);
    assert_eq!(advanced.projection.status, ExecutionStatus::Running);
    assert!(advanced.timers.is_empty());
    // Replay of the stored log reproduces the stored projection.
    let replayed = spike::replay(&advanced).unwrap();
    assert_eq!(replayed.sequence, advanced.revision);
}

#[test]
fn an_injected_fault_leaves_the_fake_untouched_and_a_retry_commits() {
    let store = store();
    let id = execution_id();
    store.fail_after(WriteStage::Events);
    assert_eq!(
        block_on(spike::start_with(&store, &id, T0)),
        Err(spike::SpikeError::Store(StoreError::Backend("injected")))
    );
    let untouched = block_on(store.load(&id)).unwrap();
    assert_eq!(untouched.revision, 0);
    assert!(untouched.events.is_empty() && untouched.timers.is_empty());
    assert!(matches!(
        block_on(spike::start_with(&store, &id, T0)).unwrap(),
        CommitResult::Committed(_)
    ));
    // A second start is a head conflict, not a second log.
    assert_eq!(
        block_on(spike::start_with(&store, &id, T0)).unwrap(),
        CommitResult::Conflict
    );
}
