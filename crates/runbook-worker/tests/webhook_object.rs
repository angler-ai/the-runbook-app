//! Gate fix D3: the WebhookDO's lifecycle. Its alarm never re-arms in the
//! past and backs off while passes fail (D1 down, no sealer key); an object
//! whose subscription D1 no longer has purges itself; and a purged log is
//! retired, so a late `Record` cannot bring it back.
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    webhooks::{
        EventRecord, OutgoingRequest, SealedSecret, SecretSealer, TransportResult, WebhookTransport,
    },
};
use runbook_model::WebhookId;
use runbook_store_d1::native::NativeD1;
use runbook_store_do::webhook::{AlarmResult, WebhookLog, alarm_at, stored_owner};
use runbook_worker::webhooks::{
    CfWebhooks, LocalWebhooks, WebhookObjects, WebhookOp, WebhookRequest,
};
use runbook_worker_conformance::native::{NativeSql, run};

const T0: i64 = 1_700_000_000_000;

fn scope() -> TenantScope {
    runbook_store_conformance::scope(1)
}
fn hook() -> WebhookId {
    format!("whk_{:026}", 7).parse().unwrap()
}
fn event() -> EventRecord {
    EventRecord {
        event_id: "evt_1".into(),
        event_type: "execution.completed".into(),
        execution_id: None,
        step_run_id: None,
        human: false,
        created_at_ms: T0,
        body: "{}".into(),
    }
}

#[test]
fn the_alarm_has_a_floor_and_backs_off_while_passes_fail() {
    let sql = NativeSql::new(T0);
    WebhookLog::open(sql.clone(), scope(), hook()).unwrap();
    let past = Some(T0 - 60_000);
    // After a pass, an overdue delivery wakes the object a second from now,
    // not now (no loop).
    assert_eq!(
        alarm_at(&sql, past, T0, AlarmResult::Succeeded).unwrap(),
        Some(T0 + 1_000)
    );
    // A write or read leaves an overdue alarm due: polling the log must not
    // keep pushing it a second later (the contract suite's starvation).
    for now in [T0, T0 + 500, T0 + 900] {
        assert_eq!(alarm_at(&sql, past, now, AlarmResult::Rearm).unwrap(), past);
    }
    // Each failed pass doubles the wait, up to five minutes.
    let mut waits = vec![];
    for _ in 0..12 {
        waits.push(
            alarm_at(&sql, past, T0, AlarmResult::Failed)
                .unwrap()
                .unwrap()
                - T0,
        );
    }
    assert_eq!(&waits[..5], &[2_000, 4_000, 8_000, 16_000, 32_000]);
    assert_eq!(*waits.last().unwrap(), 300_000);
    // A write while backing off does not cut the wait short.
    assert_eq!(
        alarm_at(&sql, past, T0, AlarmResult::Rearm).unwrap(),
        Some(T0 + 300_000)
    );
    // A pass that works resets it; nothing due, no alarm.
    assert_eq!(
        alarm_at(&sql, past, T0, AlarmResult::Succeeded).unwrap(),
        Some(T0 + 1_000)
    );
    assert_eq!(
        alarm_at(&sql, None, T0, AlarmResult::Succeeded).unwrap(),
        None
    );
    // A future due time is kept.
    assert_eq!(
        alarm_at(&sql, Some(T0 + 9_000), T0, AlarmResult::Rearm).unwrap(),
        Some(T0 + 9_000)
    );
}

#[test]
fn a_purged_log_is_retired_and_refuses_a_late_record() {
    let sql = NativeSql::new(T0);
    let log = WebhookLog::open(sql.clone(), scope(), hook()).unwrap();
    log.record(&event(), &[]).unwrap();
    assert!(stored_owner(&sql).unwrap().is_some());
    log.purge_all().unwrap();
    assert_eq!(stored_owner(&sql).unwrap(), None);
    assert!(matches!(
        WebhookLog::open(sql.clone(), scope(), hook()),
        Err(StoreError::NotFound)
    ));
    assert!(stored_owner(&sql).unwrap().is_none(), "nothing came back");
}

struct NoTransport;
impl WebhookTransport for NoTransport {
    async fn post(&self, _: OutgoingRequest) -> TransportResult {
        panic!("a retired object attempts nothing")
    }
}
struct NoSealer;
impl SecretSealer for NoSealer {
    fn seal(&self, _: &str, _: &str) -> Result<SealedSecret, StoreError> {
        Err(StoreError::Backend("test.sealer"))
    }
    fn open(&self, _: &str, _: &SealedSecret) -> Result<String, StoreError> {
        Err(StoreError::Backend("test.sealer"))
    }
}

/// Gate round 2: an object retires only on positive evidence that its
/// subscription was deleted (the marker `delete_subscription` writes in its
/// D1 batch). A D1 that merely does not list it (a briefly wrong shard map,
/// a lagging read) is a failed pass: the log stays and the alarm backs off.
#[test]
fn the_alarm_retires_only_a_subscription_d1_marked_deleted() {
    use runbook_core::webhooks::WebhookStore;
    use runbook_store_conformance::webhooks::subscription;
    let d1 = NativeD1::new(T0);
    run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
    let objects: LocalWebhooks<NativeSql> = LocalWebhooks::default();
    let record = |objects: &LocalWebhooks<NativeSql>, sub: WebhookId| {
        run(objects.call(WebhookRequest {
            scope: scope(),
            subscription: sub,
            op: WebhookOp::Record {
                event: event(),
                deliveries: vec![],
            },
        }))
    };
    record(&objects, hook()).unwrap();
    let webhooks = CfWebhooks::new(d1, objects.clone());
    // D1 does not list the subscription, and holds no deletion marker.
    let unknown = run(runbook_worker::webhooks::alarm(
        &webhooks,
        &NoTransport,
        Some(&NoSealer),
        &scope(),
        &hook(),
    ));
    assert_eq!(unknown, Err("webhooks.subscription_unknown".to_owned()));
    assert_eq!(
        record(&objects, hook()),
        Ok(serde_json::json!(false)),
        "the log is intact"
    );
    // A real deletion: D1 marks it, and the alarm retires the object.
    let s = scope();
    let sub = subscription(&s, 3, T0);
    run(webhooks.create_subscription(&s, sub.clone(), 20, None)).unwrap();
    record(&objects, sub.id.clone()).unwrap();
    assert!(run(webhooks.d1(&s).delete_subscription(&s, &sub.id)).unwrap());
    let outcome = run(runbook_worker::webhooks::alarm(
        &webhooks,
        &NoTransport,
        Some(&NoSealer),
        &s,
        &sub.id,
    ));
    assert_eq!(outcome, Ok("retired".to_owned()));
    assert_eq!(record(&objects, sub.id.clone()), Err(StoreError::NotFound));
}

/// Gate round 2: a retired subscription's object (deleted, purged) is
/// skipped by fan-out and the other per-subscription passes; the event and
/// every other subscription's deliveries still go through.
#[test]
fn fan_out_skips_a_retired_subscription() {
    use runbook_core::webhooks::{WebhookStore, new_delivery};
    use runbook_store_conformance::webhooks::{subscription, whd};
    let d1 = NativeD1::new(T0);
    run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
    let objects: LocalWebhooks<NativeSql> = LocalWebhooks::default();
    let webhooks = CfWebhooks::new(d1, objects.clone());
    let s = scope();
    let (one, two) = (subscription(&s, 1, T0), subscription(&s, 2, T0));
    run(webhooks.create_subscription(&s, one.clone(), 20, None)).unwrap();
    run(webhooks.create_subscription(&s, two.clone(), 20, None)).unwrap();
    // Subscription one's object was retired while D1 still lists it.
    run(objects.call(WebhookRequest {
        scope: s.clone(),
        subscription: one.id.clone(),
        op: WebhookOp::PurgeAll,
    }))
    .ok();
    run(objects.call(WebhookRequest {
        scope: s.clone(),
        subscription: one.id.clone(),
        op: WebhookOp::Record {
            event: event(),
            deliveries: vec![],
        },
    }))
    .ok();
    let mut e = event();
    e.execution_id = Some("exe_00000000000000000000000001".parse().unwrap());
    let deliveries = vec![
        new_delivery(whd(11), &one, &e, None, T0),
        new_delivery(whd(12), &two, &e, None, T0),
    ];
    run(webhooks.record_event(&s, e.clone(), deliveries, None)).unwrap();
    assert!(run(webhooks.delivery(&s, &whd(12))).unwrap().is_some());
    let execution = e.execution_id.clone().unwrap();
    run(webhooks.execution_events(&s, &execution)).unwrap();
    run(webhooks.purge_delivery_log(&s, T0 + 1, 10)).unwrap();
    run(webhooks.forget_execution(&s, &execution)).unwrap();
}

/// Gate fix (low): a redelivery writes its replay row first (the guarded
/// commit point), `Add` is idempotent by delivery ID, and an `Add` that
/// fails takes the replay row back so a retry starts clean.
#[test]
fn a_redelivery_commits_at_its_replay_row_and_retries_cleanly() {
    use runbook_core::webhooks::{WebhookReplay, WebhookStore, new_delivery};
    use runbook_store_conformance::webhooks::{event, subscription, whd};
    let d1 = NativeD1::new(T0);
    run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
    let objects: LocalWebhooks<NativeSql> = LocalWebhooks::default();
    let webhooks = CfWebhooks::new(d1, objects.clone());
    let s = scope();
    let sub = subscription(&s, 1, T0);
    run(webhooks.create_subscription(&s, sub.clone(), 20, None)).unwrap();
    let e = event("evt_redeliver", T0);
    let original = new_delivery(whd(1), &sub, &e, None, T0);
    run(webhooks.record_event(&s, e.clone(), vec![original.clone()], None)).unwrap();
    let replay = |key: &str| WebhookReplay {
        key: key.into(),
        request_hash: [3; 32],
        response: serde_json::Value::Null,
        created_at_ms: T0,
    };
    let again = new_delivery(whd(2), &sub, &e, Some(whd(1)), T0);
    run(webhooks.add_delivery(&s, again.clone(), Some(replay("r1")))).unwrap();
    // The same delivery again (a retry after the replay row was lost to a
    // later failure) is not a duplicate.
    run(webhooks.add_delivery(&s, again.clone(), None)).unwrap();
    assert_eq!(
        run(webhooks.add_delivery(
            &s,
            new_delivery(whd(3), &sub, &e, Some(whd(1)), T0),
            Some(replay("r1"))
        )),
        Err(StoreError::InvalidBatch("webhooks.replay_exists"))
    );
    assert!(
        run(webhooks.delivery(&s, &whd(3))).unwrap().is_none(),
        "never added"
    );
    // An event the subscription's object does not hold: the Add fails, and
    // its replay row is taken back.
    let orphan = event("evt_orphan", T0);
    run(webhooks.d1(&s).record_event_row(&s, &orphan, None)).unwrap();
    assert!(
        run(webhooks.add_delivery(
            &s,
            new_delivery(whd(4), &sub, &orphan, Some(whd(1)), T0),
            Some(replay("r2"))
        ))
        .is_err()
    );
    assert_eq!(run(webhooks.find_replay(&s, "r2")).unwrap(), None);
}

/// The subscriptions' objects, losing the next Add with an ambiguous error
/// (the Add may or may not have happened).
#[derive(Clone)]
struct LossyObjects {
    inner: LocalWebhooks<NativeSql>,
    lose_add: std::rc::Rc<std::cell::Cell<bool>>,
}
impl WebhookObjects for LossyObjects {
    async fn call(&self, request: WebhookRequest) -> runbook_worker::protocol::Reply {
        if matches!(request.op, WebhookOp::Add { .. }) && self.lose_add.replace(false) {
            return Err(StoreError::Backend("test.connection_lost"));
        }
        self.inner.call(request).await
    }
}

/// Gate round 2: a redelivery whose Add failed ambiguously keeps its replay
/// row, and the retry that finds that row checks the object for its
/// delivery and adds it if it is missing (Add is idempotent by ID), so the
/// retry neither adds a second delivery nor answers for a missing one.
#[test]
fn an_ambiguous_redelivery_keeps_its_replay_and_the_retry_completes_it() {
    use runbook_core::webhooks::{WebhookReplay, WebhookStore, new_delivery};
    use runbook_store_conformance::webhooks::{event, subscription, whd};
    let d1 = NativeD1::new(T0);
    run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
    let objects = LossyObjects {
        inner: LocalWebhooks::default(),
        lose_add: Default::default(),
    };
    let webhooks = CfWebhooks::new(d1, objects.clone());
    let s = scope();
    let sub = subscription(&s, 1, T0);
    run(webhooks.create_subscription(&s, sub.clone(), 20, None)).unwrap();
    let e = event("evt_ambiguous", T0);
    run(webhooks.record_event(
        &s,
        e.clone(),
        vec![new_delivery(whd(1), &sub, &e, None, T0)],
        None,
    ))
    .unwrap();
    let again = new_delivery(whd(9), &sub, &e, Some(whd(1)), T0);
    let replay = WebhookReplay {
        key: "r9".into(),
        request_hash: [5; 32],
        response: serde_json::json!(again.view(&[])),
        created_at_ms: T0,
    };
    objects.lose_add.set(true);
    assert!(run(webhooks.add_delivery(&s, again.clone(), Some(replay.clone()))).is_err());
    // The replay row stays; the retry finds it, and the delivery is there.
    assert_eq!(run(webhooks.find_replay(&s, "r9")).unwrap(), Some(replay));
    let found = run(webhooks.delivery(&s, &whd(9)))
        .unwrap()
        .map(|(d, _)| d.redelivery_of);
    assert_eq!(found, Some(Some(whd(1))));
    let page = run(webhooks.deliveries(&s, &sub.id, 50, None)).unwrap();
    assert_eq!(page.len(), 2, "one redelivery, not two");
}
