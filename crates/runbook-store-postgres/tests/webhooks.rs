//! C1: the shared webhook store cases on PostgreSQL, plus a concurrent
//! limit race and a lease that expires on the database clock.
mod support;
use futures_util::FutureExt;
use runbook_core::webhooks::WebhookStore;
use runbook_store_postgres::{PostgresStore, PostgresWebhooks};
use std::panic::AssertUnwindSafe;
use support::database::TestDatabase;

async fn with_webhooks<F>(case: F)
where
    F: AsyncFnOnce(&PostgresWebhooks, &TestDatabase),
{
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut admin = PostgresStore::connect(db.config()).await.unwrap();
        admin.migrate().await.unwrap();
        let store = PostgresWebhooks::new(db.config().clone());
        case(&store, &db).await;
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

macro_rules! shared {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_webhooks(async |w, _| {
                runbook_store_conformance::webhooks::$name(w).await.unwrap()
            })
            .await;
        }
    )*};
}
shared!(
    limit_is_per_project_and_tenants_are_isolated,
    fan_out_is_idempotent_and_claims_are_fenced,
    replay_rows_and_delete_cascade,
    stale_saves_conflict,
    delivery_log_purge_is_bounded_and_keeps_verdicts,
);

/// Independent pools racing for the last two slots: the per-project
/// advisory lock lets exactly two through.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn concurrent_creates_cannot_exceed_the_limit() {
    use runbook_core::webhooks::{SealedSecret, Subscription};
    with_webhooks(async |w, db| {
        let at = w.webhook_time_ms().await.unwrap();
        let scope = runbook_store_conformance::scope(1);
        let make = |n: u32| Subscription {
            id: format!("whk_{n:026}").parse().unwrap(),
            scope: scope.clone(),
            name: format!("hook {n}"),
            url: "https://hooks.example/x".into(),
            events: vec!["*".into()],
            runbooks: vec![],
            tags: vec![],
            enabled: true,
            secret: SealedSecret("sealed".into()),
            previous_secret: None,
            created_at_ms: at,
            updated_at_ms: at,
            secret_rotated_at_ms: at,
            failing_since_ms: None,
            last_failure_at_ms: None,
            disabled_reason: None,
            disabled_at_ms: None,
            revision: 0,
            notice: None,
        };
        for n in 0..18 {
            w.create_subscription(&scope, make(n), 20, None)
                .await
                .unwrap();
        }
        // Six workers on warmed, independent pools race for two free slots.
        let workers: Vec<PostgresWebhooks> = (0..6)
            .map(|_| PostgresWebhooks::new(db.config().clone()))
            .collect();
        for worker in &workers {
            worker.webhook_time_ms().await.unwrap();
        }
        let results =
            futures_util::future::join_all(workers.iter().enumerate().map(|(i, worker)| {
                worker.create_subscription(&scope, make(100 + i as u32), 20, None)
            }))
            .await;
        let ok = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok, 2, "{results:?}");
        assert_eq!(w.list_subscriptions(&scope).await.unwrap().len(), 20);
    })
    .await;
}

/// F31: the delivery-log purge also removes recorded events older than the
/// cutoff once no delivery references them, and D20 replay rows past their
/// 24 hours. Events with a kept delivery, recent events and live replays stay.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn purge_removes_old_unreferenced_events_and_expired_replays() {
    use runbook_core::webhooks::{
        DELIVERY_LOG_RETENTION_MS, EventRecord, SealedSecret, Subscription, WebhookReplay,
        new_delivery,
    };
    with_webhooks(async |w, db| {
        let now = w.webhook_time_ms().await.unwrap();
        let old = now - DELIVERY_LOG_RETENTION_MS - 86_400_000;
        let scope = runbook_store_conformance::scope(1);
        let other = runbook_store_conformance::scope(2);
        let subscription = Subscription {
            id: format!("whk_{:026}", 1).parse().unwrap(),
            scope: scope.clone(),
            name: "hook".into(),
            url: "https://hooks.example/1".into(),
            events: vec!["*".into()],
            runbooks: vec![],
            tags: vec![],
            enabled: true,
            secret: SealedSecret("sealed".into()),
            previous_secret: None,
            created_at_ms: old,
            updated_at_ms: old,
            secret_rotated_at_ms: old,
            failing_since_ms: None,
            disabled_reason: None,
            disabled_at_ms: None,
            last_failure_at_ms: None,
            revision: 0,
            notice: None,
        };
        w.create_subscription(&scope, subscription.clone(), 20, None)
            .await
            .unwrap();
        let event = |id: &str, at: i64| EventRecord {
            event_id: id.into(),
            event_type: "step.completed".into(),
            execution_id: Some("exe_00000000000000000000000001".parse().unwrap()),
            step_run_id: None,
            human: false,
            created_at_ms: at,
            body: format!("{{\"id\":\"{id}\"}}"),
        };
        let replay = |key: &str, at: i64| WebhookReplay {
            key: key.into(),
            request_hash: [1; 32],
            response: serde_json::json!({}),
            created_at_ms: at,
        };
        // Old with no delivery (no subscriber matched): purged.
        w.record_event(
            &scope,
            event("evt_1", old),
            vec![],
            Some(replay("old", old)),
        )
        .await
        .unwrap();
        // Old with a pending delivery: the delivery and so the event stay.
        let mut waiting = new_delivery(
            format!("whd_{:026}", 2).parse().unwrap(),
            &subscription,
            &event("evt_2", old),
            None,
            old,
        );
        waiting.next_attempt_at_ms = Some(now + 3_600_000);
        w.record_event(&scope, event("evt_2", old), vec![waiting], None)
            .await
            .unwrap();
        // Recent: stays, with its live replay.
        w.record_event(
            &scope,
            event("evt_3", now),
            vec![],
            Some(replay("new", now)),
        )
        .await
        .unwrap();
        // Another tenant's old event waits for its own purge.
        w.record_event(
            &other,
            event("evt_4", old),
            vec![],
            Some(replay("old", old)),
        )
        .await
        .unwrap();
        while w
            .purge_delivery_log(&scope, now - DELIVERY_LOG_RETENTION_MS, 1)
            .await
            .unwrap()
            > 0
        {}
        assert!(w.get_event(&scope, "evt_1").await.unwrap().is_none());
        assert!(w.get_event(&scope, "evt_2").await.unwrap().is_some());
        assert!(w.get_event(&scope, "evt_3").await.unwrap().is_some());
        assert!(w.get_event(&other, "evt_4").await.unwrap().is_some());
        let admin = PostgresStore::connect(db.config()).await.unwrap();
        let keys = |org: &str| {
            let org = org.to_owned();
            let admin = &admin;
            async move {
                admin
                    .client()
                    .query(
                        "SELECT replay_key FROM runbook.webhook_replay \
                         WHERE org_id = $1 ORDER BY replay_key",
                        &[&org],
                    )
                    .await
                    .unwrap()
                    .iter()
                    .map(|r| r.get::<_, String>(0))
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(keys(scope.org_id.as_str()).await, ["new"]);
        assert_eq!(keys(other.org_id.as_str()).await, ["old"]);
    })
    .await;
}

/// F34: a completion and a subscription delete lock in one order
/// (subscription, then delivery), so they serialize instead of deadlocking.
/// A table lock parks the completion after it has locked its rows; the
/// delete then queues behind it before the lock is released.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn completion_and_subscription_delete_do_not_deadlock() {
    use runbook_core::webhooks::{
        AttemptCompletion, AttemptRecord, EventRecord, SealedSecret, Subscription, new_delivery,
    };
    use runbook_model::{AttemptOutcome, DeliveryState};
    with_webhooks(async |w, db| {
        let now = w.webhook_time_ms().await.unwrap();
        let scope = runbook_store_conformance::scope(1);
        let subscription = Subscription {
            id: format!("whk_{:026}", 1).parse().unwrap(),
            scope: scope.clone(),
            name: "hook".into(),
            url: "https://hooks.example/1".into(),
            events: vec!["*".into()],
            runbooks: vec![],
            tags: vec![],
            enabled: true,
            secret: SealedSecret("sealed".into()),
            previous_secret: None,
            created_at_ms: now,
            updated_at_ms: now,
            secret_rotated_at_ms: now,
            failing_since_ms: None,
            disabled_reason: None,
            disabled_at_ms: None,
            last_failure_at_ms: None,
            revision: 0,
            notice: None,
        };
        w.create_subscription(&scope, subscription.clone(), 20, None)
            .await
            .unwrap();
        let event = EventRecord {
            event_id: "evt_1".into(),
            event_type: "step.completed".into(),
            execution_id: None,
            step_run_id: None,
            human: false,
            created_at_ms: now,
            body: "{}".into(),
        };
        let id: runbook_model::DeliveryId = format!("whd_{:026}", 1).parse().unwrap();
        let delivery = new_delivery(id.clone(), &subscription, &event, None, now);
        w.record_event(&scope, event, vec![delivery], None)
            .await
            .unwrap();
        let lease = w
            .claim_deliveries(&scope, 1, 60_000)
            .await
            .unwrap()
            .remove(0);
        let completion = AttemptCompletion {
            attempt: AttemptRecord {
                delivery_id: id.clone(),
                number: lease.delivery.attempts + 1,
                at_ms: now,
                status: Some(200),
                latency_ms: 1,
                outcome: AttemptOutcome::Delivered,
                error: None,
                snippet: None,
            },
            state: DeliveryState::Delivered,
            next_attempt_at_ms: None,
            failure: None,
            health: Some(true),
        };
        let admin = PostgresStore::connect(db.config()).await.unwrap();
        admin
            .client()
            .batch_execute("BEGIN; LOCK TABLE runbook.webhook_attempt IN SHARE MODE")
            .await
            .unwrap();
        // Activity is read outside the locking transaction: a transaction
        // keeps one snapshot of pg_stat_activity until it ends.
        let observer = PostgresStore::connect(db.config()).await.unwrap();
        let waiting = async || -> i64 {
            observer
                .client()
                .query_one(
                    "SELECT count(*) FROM pg_stat_activity \
                     WHERE datname = current_database() AND wait_event_type = 'Lock'",
                    &[],
                )
                .await
                .unwrap()
                .get(0)
        };
        // Bounded: a missed interleaving fails instead of hanging.
        let until = async |n: i64| {
            for _ in 0..1_000 {
                if waiting().await >= n {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            false
        };
        let complete = w.complete_attempt(&scope, &id, lease.generation, completion);
        let delete = async {
            // Start the delete once the completion is parked on the table lock.
            assert!(until(1).await, "completion never waited");
            w.delete_subscription(&scope, &subscription.id).await
        };
        let release = async {
            let parked = until(2).await;
            admin.client().batch_execute("ROLLBACK").await.unwrap();
            assert!(parked, "delete never waited");
        };
        let (completed, deleted, ()) = tokio::join!(complete, delete, release);
        assert_eq!(completed, Ok(()));
        assert_eq!(deleted, Ok(true));
        assert_eq!(w.delivery(&scope, &id).await.unwrap(), None);
    })
    .await;
}
