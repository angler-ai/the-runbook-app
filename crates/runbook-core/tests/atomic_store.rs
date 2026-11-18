use runbook_core::*;
use runbook_engine::{Event, EventEnvelope, ExecutionPhase, NotificationIntent};
use runbook_model::{ActorRef, ExecutionStatus};
fn projection() -> ExecutionProjection {
    ExecutionProjection {
        execution_id: "exe_00000000000000000000000001".parse().unwrap(),
        runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
        version_id: "rbv_00000000000000000000000001".parse().unwrap(),
        compiled_hash: [7; 32],
        engine_version: runbook_engine::ENGINE_VERSION,
        sequence: 0,
        status: ExecutionStatus::Queued,
        phase: ExecutionPhase::Ordinary,
        completed_at_ms: None,
        result: serde_json::Value::Null,
        outputs: Default::default(),
        steps: vec![],
    }
}
fn batch() -> ExecutionCommit {
    let mut projection = projection();
    projection.sequence = 1;
    projection.status = ExecutionStatus::Running;
    ExecutionCommit {
        execution_id: projection.execution_id.clone(),
        expected_sequence: 0,
        events: vec![EventEnvelope {
            execution_id: projection.execution_id.clone(),
            seq: 1,
            at_ms: 10,
            actor: ActorRef::System,
            event: Event::ManualPauseChanged { paused: false },
        }],
        timers: vec![TimerChange::Upsert(TimerRecord {
            execution_id: projection.execution_id.clone(),
            key: "execution".into(),
            generation: 1,
            due_at_ms: 100,
        })],
        outbox: vec![NotificationIntent {
            execution_id: projection.execution_id.clone(),
            event_id: "evt_00000000000000000000000001".into(),
            kind: "execution.started".into(),
            sequence: 1,
            at_ms: 10,
            step_run_ids: vec![],
            recipients: vec![],
            reason: None,
            link: "https://example.test/run".into(),
            ready_steps: vec![],
            params: None,
            detail: None,
        }],
        projection,
        idempotency: vec![],
        claims: vec![],
        credentials: vec![],
        derived_outbox: vec![],
    }
}
#[test]
fn commit_batch_is_plain_data() {
    fn plain<T: Clone + PartialEq + serde::Serialize + for<'de> serde::Deserialize<'de>>() {}
    plain::<ExecutionCommit>();
    let batch = batch();
    let encoded = serde_json::to_vec(&batch).unwrap();
    assert_eq!(
        serde_json::from_slice::<ExecutionCommit>(&encoded).unwrap(),
        batch
    );
}

#[path = "support/do_fake.rs"]
mod do_fake;
fn complete<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("synchronous transaction fake yielded"),
    }
}
#[test]
fn non_send_store_can_commit() {
    let store = do_fake::FakeStore::new(scope(), projection(), 10);
    let id = projection().execution_id;
    assert_eq!(complete(store.load(&id)).unwrap().revision, 0);
    assert_eq!(
        complete(store.commit(batch())).unwrap(),
        CommitResult::Committed(1)
    );
    let stored = complete(store.load(&id)).unwrap();
    assert_eq!(stored.revision, 1);
    assert_eq!(stored.projection, batch().projection);
    assert_eq!(stored.timers.len(), 1);
    assert_eq!(stored.outbox.len(), 1);
}
#[test]
fn injected_commit_failure_changes_nothing() {
    let store = do_fake::FakeStore::new(scope(), projection(), 10);
    let id = projection().execution_id;
    let before = complete(store.load(&id)).unwrap();
    store.fail_after_events();
    assert_eq!(
        complete(store.commit(batch())),
        Err(StoreError::Backend("injected"))
    );
    let after = complete(store.load(&id)).unwrap();
    assert_eq!(after.events, before.events);
    assert_eq!(after.projection, before.projection);
    assert_eq!(after.outbox, before.outbox);
    assert_eq!(after.timers, before.timers);
    assert_eq!(after.revision, before.revision);
}
#[test]
fn stale_writer_cannot_change_any_execution_owned_record() {
    let store = do_fake::FakeStore::new(scope(), projection(), 10);
    let second = store.clone();
    let id = projection().execution_id;
    let a = complete(store.load(&id)).unwrap();
    let b = complete(second.load(&id)).unwrap();
    assert_eq!(a.revision, b.revision);
    assert_eq!(
        complete(store.commit(batch())).unwrap(),
        CommitResult::Committed(1)
    );
    let committed = complete(store.load(&id)).unwrap();
    let mut stale = batch();
    stale.projection.result = serde_json::json!("stale");
    assert_eq!(
        complete(second.commit(stale)).unwrap(),
        CommitResult::Conflict
    );
    assert_eq!(complete(store.load(&id)).unwrap(), committed);
}
#[test]
fn malformed_or_cross_execution_writes_are_atomic_rejections() {
    let foreign: runbook_model::ExecutionId = "exe_00000000000000000000000002".parse().unwrap();
    let mut variants = vec![];
    let mut bad = batch();
    bad.events[0].execution_id = foreign.clone();
    variants.push(bad);
    let mut bad = batch();
    bad.events[0].seq = 2;
    variants.push(bad);
    let mut bad = batch();
    bad.projection.execution_id = foreign.clone();
    variants.push(bad);
    let mut bad = batch();
    bad.outbox[0].execution_id = foreign.clone();
    variants.push(bad);
    let mut bad = batch();
    bad.timers.push(TimerChange::Delete {
        execution_id: foreign,
        key: "other".into(),
    });
    variants.push(bad);
    let mut bad = batch();
    bad.projection.steps.push(StepProjection {
        execution_id: "exe_00000000000000000000000002".parse().unwrap(),
        step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
        path: "work".into(),
        status: runbook_model::StepStatus::Ready,
        verdict: None,
        outputs: Default::default(),
        summary: None,
    });
    variants.push(bad);
    let mut bad = batch();
    bad.events.clear();
    variants.push(bad);
    let mut bad = batch();
    bad.projection.completed_at_ms = Some(10);
    variants.push(bad);
    let mut bad = batch();
    bad.timers.push(bad.timers[0].clone());
    variants.push(bad);
    let mut bad = batch();
    bad.outbox.push(bad.outbox[0].clone());
    variants.push(bad);
    let mut bad = batch();
    bad.projection.compiled_hash = [8; 32];
    variants.push(bad);
    for bad in variants {
        let store = do_fake::FakeStore::new(scope(), projection(), 10);
        let id = projection().execution_id;
        let before = complete(store.load(&id)).unwrap();
        assert!(matches!(
            complete(store.commit(bad)),
            Err(StoreError::InvalidBatch(_))
        ));
        assert_eq!(complete(store.load(&id)).unwrap(), before);
    }
}
#[test]
fn timer_deletion_and_completion_commit_together() {
    let store = do_fake::FakeStore::new(scope(), projection(), 10);
    complete(store.commit(batch())).unwrap();
    let mut done = batch();
    done.expected_sequence = 1;
    done.events[0].seq = 2;
    done.events[0].at_ms = 20;
    done.projection.sequence = 2;
    done.projection.status = ExecutionStatus::Succeeded;
    done.projection.completed_at_ms = Some(20);
    done.projection.result = serde_json::json!("healthy");
    done.timers = vec![TimerChange::Delete {
        execution_id: done.execution_id.clone(),
        key: "execution".into(),
    }];
    done.outbox.clear();
    assert_eq!(
        complete(store.commit(done)).unwrap(),
        CommitResult::Committed(2)
    );
    let stored = complete(store.load(&projection().execution_id)).unwrap();
    assert!(stored.timers.is_empty());
    assert_eq!(stored.outbox.len(), 1);
    assert_eq!(stored.projection.completed_at_ms, Some(20));
}
#[test]
fn new_execution_creation_does_not_touch_other_runs() {
    let store = do_fake::FakeStore::new(scope(), projection(), 10);
    complete(store.commit(batch())).unwrap();
    let before = complete(store.load(&projection().execution_id)).unwrap();
    let foreign: runbook_model::ExecutionId = "exe_00000000000000000000000002".parse().unwrap();
    assert_eq!(complete(store.load(&foreign)), Err(StoreError::NotFound));
    let mut other = batch();
    other.execution_id = foreign.clone();
    other.projection.execution_id = foreign.clone();
    other.events[0].execution_id = foreign.clone();
    other.outbox[0].execution_id = foreign.clone();
    other.outbox[0].event_id = "evt_00000000000000000000000002".into();
    let TimerChange::Upsert(timer) = &mut other.timers[0] else {
        unreachable!()
    };
    timer.execution_id = foreign.clone();
    assert_eq!(
        complete(store.commit(other)).unwrap(),
        CommitResult::Committed(1)
    );
    assert_eq!(
        complete(store.load(&projection().execution_id)).unwrap(),
        before
    );
    assert_eq!(
        complete(store.load(&foreign))
            .unwrap()
            .projection
            .execution_id,
        foreign
    );
}
#[test]
fn projections_preserve_exact_json_in_storage() {
    let mut batch = batch();
    batch.projection.outputs.insert(
        "number".into(),
        serde_json::from_str("9007199254740993.0").unwrap(),
    );
    batch.projection.outputs.insert(
        "literal".into(),
        serde_json::json!({"$serde_json::private::Number":"not-a-number"}),
    );
    let encoded = serde_json::to_vec(&batch).unwrap();
    assert_eq!(
        serde_json::from_slice::<ExecutionCommit>(&encoded).unwrap(),
        batch
    );
}

fn scope() -> runbook_core::catalog::TenantScope {
    runbook_core::catalog::TenantScope {
        org_id: "org_00000000000000000000000001".parse().unwrap(),
        project_id: "prj_00000000000000000000000001".parse().unwrap(),
    }
}
