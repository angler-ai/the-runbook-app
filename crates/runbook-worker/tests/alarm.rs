//! The ExecutionDO alarm handler (`spike::run_alarm`) over the real DO
//! store on native SQLite (CF2 review fix 4).
use runbook_core::{
    ExecutionStore, TimerChange, TimerRecord,
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::ExecutionId;
use runbook_store_do::DoStore;
use runbook_worker::spike;
use runbook_worker_conformance::native::{NativeSql, run};

const T0: i64 = 1_700_000_000_000;

fn started() -> (DoStore<NativeSql>, NativeSql, ExecutionId) {
    let id: ExecutionId = "exe_00000000000000000000000001".parse().unwrap();
    let sql = NativeSql::new(T0);
    let store = DoStore::open(sql.clone(), &id, spike::scope()).unwrap();
    run(spike::start_with(&store, &id, T0)).unwrap();
    (store, sql, id)
}

#[test]
fn the_alarm_leases_only_the_timers_it_fires() {
    let (store, sql, _) = started();
    sql.set_time(T0 + 1_000);
    let outcome = run(spike::run_alarm(&store)).unwrap();
    assert!(outcome.starts_with("advanced:"), "{outcome}");
    // No notification was leased, the start's included: each consumer
    // gets them at generation 1.
    let pending = store.pending_notifications(None, 100).unwrap();
    assert!(
        pending.iter().any(|i| i.sequence <= 3),
        "the start's intent"
    );
    let rest = run(store.claim_due(100, 60_000)).unwrap();
    let notifications: Vec<_> = rest
        .iter()
        .filter(|l| matches!(l.id.kind, WorkKind::Notification { .. }))
        .collect();
    assert_eq!(notifications.len(), pending.len(), "{rest:?}");
    assert!(notifications.iter().all(|l| l.generation == 1), "{rest:?}");
}

#[test]
fn a_timer_the_advance_does_not_fire_keeps_its_lease() {
    let (store, sql, id) = started();
    // A stored timer the engine does not know: due, but nothing advances.
    // It rides the wait's own advance (a no-event batch may carry no timer).
    let run_state = run(store.load(&id)).unwrap();
    let mut with_event = spike::advance_commit(&run_state, T0 + 1_000)
        .unwrap()
        .expect("the wait fires");
    with_event.timers.push(TimerChange::Upsert(TimerRecord {
        execution_id: id.clone(),
        key: "orphan".into(),
        generation: 1,
        due_at_ms: T0,
    }));
    sql.set_time(T0 + 1_000);
    run(store.commit(with_event)).unwrap();
    let outcome = run(spike::run_alarm(&store)).unwrap();
    assert!(outcome.contains("not_due"), "{outcome}");
    // Not acknowledged: once its lease lapses the orphan is offered again.
    sql.set_time(T0 + 1_000 + spike::ALARM_LEASE_MS as i64);
    let again = store
        .claim_due_of(10, 60_000, |k| matches!(k, WorkKind::Timer { .. }))
        .unwrap();
    assert!(
        again.iter().any(
            |l| matches!(&l.id.kind, WorkKind::Timer { key, .. } if key == "orphan")
                && l.generation == 2
        ),
        "{again:?}"
    );
}
