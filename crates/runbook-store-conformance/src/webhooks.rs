//! C1 webhook store cases shared by the memory and Postgres adapters:
//! per-project limit, tenant isolation, idempotent fan-out, fenced delivery
//! claims with atomic health, replay rows and delete cascade.
use crate::scope;
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    webhooks::{
        AttemptCompletion, AttemptRecord, DeliveryRecord, EventRecord, SealedSecret, Subscription,
        WebhookReplay, WebhookStore, new_delivery,
    },
};
use runbook_model::{AttemptOutcome, DeliveryId, DeliveryState, WebhookId};

pub fn whk(n: u32) -> WebhookId {
    format!("whk_{n:026}").parse().unwrap()
}
pub fn whd(n: u32) -> DeliveryId {
    format!("whd_{n:026}").parse().unwrap()
}
pub fn subscription(scope: &TenantScope, n: u32, at: i64) -> Subscription {
    Subscription {
        id: whk(n),
        scope: scope.clone(),
        name: format!("hook {n}"),
        url: format!("https://hooks.example/{n}"),
        events: vec!["*".into()],
        runbooks: vec![],
        tags: vec![],
        enabled: true,
        secret: SealedSecret(format!("sealed-{n}")),
        previous_secret: None,
        created_at_ms: at + i64::from(n),
        updated_at_ms: at,
        secret_rotated_at_ms: at,
        failing_since_ms: None,
        last_failure_at_ms: None,
        disabled_reason: None,
        disabled_at_ms: None,
        revision: 0,
        notice: None,
    }
}
pub fn event(id: &str, at: i64) -> EventRecord {
    EventRecord {
        event_id: id.into(),
        event_type: "approval.requested".into(),
        execution_id: Some("exe_00000000000000000000000001".parse().unwrap()),
        step_run_id: None,
        human: true,
        created_at_ms: at,
        body: format!("{{\"id\":\"{id}\"}}"),
    }
}
fn delivered(lease_delivery: &DeliveryRecord, at: i64, ok: bool) -> AttemptCompletion {
    AttemptCompletion {
        attempt: AttemptRecord {
            delivery_id: lease_delivery.id.clone(),
            number: lease_delivery.attempts + 1,
            at_ms: at,
            status: Some(if ok { 200 } else { 500 }),
            latency_ms: 1,
            outcome: if ok {
                AttemptOutcome::Delivered
            } else {
                AttemptOutcome::Failed
            },
            error: None,
            snippet: None,
        },
        state: if ok {
            DeliveryState::Delivered
        } else {
            DeliveryState::Failed
        },
        next_attempt_at_ms: None,
        failure: (!ok).then(|| "http_500".into()),
        health: Some(ok),
    }
}

pub async fn limit_is_per_project_and_tenants_are_isolated<W: WebhookStore>(
    w: &W,
) -> Result<(), StoreError> {
    let at = w.webhook_time_ms().await?;
    let (one, two) = (scope(1), scope(2));
    for n in 0..20 {
        w.create_subscription(&one, subscription(&one, n, at), 20, None)
            .await?;
    }
    assert_eq!(
        w.create_subscription(&one, subscription(&one, 99, at), 20, None)
            .await,
        Err(StoreError::InvalidBatch("webhooks.limit_reached"))
    );
    w.create_subscription(&two, subscription(&two, 100, at), 20, None)
        .await?;
    assert_eq!(w.list_subscriptions(&one).await?.len(), 20);
    assert_eq!(w.list_subscriptions(&two).await?.len(), 1);
    assert_eq!(w.get_subscription(&two, &whk(1)).await?, None);
    assert!(!w.delete_subscription(&two, &whk(1)).await?);
    assert_eq!(
        w.save_subscription(&two, subscription(&two, 1, at), None)
            .await,
        Err(StoreError::NotFound)
    );
    assert!(w.get_subscription(&one, &whk(1)).await?.is_some());
    // A foreign scope's subscription cannot receive this tenant's deliveries.
    let e = event("evt_foreign", at);
    let d = new_delivery(whd(1), &subscription(&two, 100, at), &e, None, at);
    assert!(w.record_event(&one, e, vec![d], None).await.is_err());
    assert_eq!(w.get_event(&one, "evt_foreign").await?, None);
    Ok(())
}

pub async fn fan_out_is_idempotent_and_claims_are_fenced<W: WebhookStore>(
    w: &W,
) -> Result<(), StoreError> {
    let at = w.webhook_time_ms().await?;
    let s = scope(1);
    let sub = subscription(&s, 1, at);
    w.create_subscription(&s, sub.clone(), 20, None).await?;
    let e = event("evt_1", at);
    let first = new_delivery(whd(1), &sub, &e, None, at);
    assert!(
        w.record_event(&s, e.clone(), vec![first.clone()], None)
            .await?
    );
    // A retried fan-out records nothing more.
    let again = new_delivery(whd(2), &sub, &e, None, at);
    assert!(!w.record_event(&s, e.clone(), vec![again], None).await?);
    assert_eq!(w.get_event(&s, "evt_1").await?, Some(e.clone()));
    assert_eq!(w.get_event(&scope(2), "evt_1").await?, None);
    // Redeliveries are additional rows of the same event.
    let redelivery = new_delivery(whd(3), &sub, &e, Some(whd(1)), at);
    w.add_delivery(&s, redelivery, None).await?;
    assert_eq!(w.deliveries(&s, &whk(1), 10, None).await?.len(), 2);
    assert!(w.add_delivery(&s, first.clone(), None).await.is_err());

    let leases = w.claim_deliveries(&s, 10, 60_000).await?;
    assert_eq!(leases.len(), 2);
    assert!(w.claim_deliveries(&s, 10, 60_000).await?.is_empty());
    assert!(w.claim_deliveries(&scope(2), 10, 60_000).await?.is_empty());
    let lease = leases.iter().find(|l| l.delivery.id == whd(1)).unwrap();
    assert_eq!(lease.body, e.body);
    assert_eq!(
        w.complete_attempt(
            &s,
            &whd(1),
            lease.generation + 1,
            delivered(&lease.delivery, at, false)
        )
        .await,
        Err(StoreError::InvalidBatch("webhooks.stale_lease"))
    );
    assert_eq!(
        w.complete_attempt(
            &scope(2),
            &whd(1),
            lease.generation,
            delivered(&lease.delivery, at, false)
        )
        .await,
        Err(StoreError::NotFound)
    );
    w.complete_attempt(
        &s,
        &whd(1),
        lease.generation,
        delivered(&lease.delivery, at, false),
    )
    .await?;
    assert_eq!(
        w.complete_attempt(
            &s,
            &whd(1),
            lease.generation,
            delivered(&lease.delivery, at, false)
        )
        .await,
        Err(StoreError::InvalidBatch("webhooks.stale_lease"))
    );
    // Health is written with the attempt.
    let failing = w.get_subscription(&s, &whk(1)).await?.unwrap();
    assert_eq!(failing.failing_since_ms, Some(at));
    let (record, attempts) = w.delivery(&s, &whd(1)).await?.unwrap();
    assert_eq!(record.state, DeliveryState::Failed);
    assert_eq!(attempts.len(), 1);
    assert_eq!(w.delivery(&scope(2), &whd(1)).await?, None);
    let events = w
        .execution_events(&s, e.execution_id.as_ref().unwrap())
        .await?;
    assert_eq!(events.len(), 1);
    let mut states = events[0].1.clone();
    states.sort();
    assert_eq!(states, [DeliveryState::Pending, DeliveryState::Failed]);
    let other = leases.iter().find(|l| l.delivery.id == whd(3)).unwrap();
    w.complete_attempt(
        &s,
        &whd(3),
        other.generation,
        delivered(&other.delivery, at, true),
    )
    .await?;
    assert_eq!(
        w.get_subscription(&s, &whk(1))
            .await?
            .unwrap()
            .failing_since_ms,
        None
    );
    // Finished deliveries are never claimed again.
    assert!(w.claim_deliveries(&s, 10, 60_000).await?.is_empty());
    Ok(())
}

pub async fn replay_rows_and_delete_cascade<W: WebhookStore>(w: &W) -> Result<(), StoreError> {
    let at = w.webhook_time_ms().await?;
    let s = scope(1);
    let replay = WebhookReplay {
        key: "k".into(),
        request_hash: [7; 32],
        response: serde_json::Value::Null,
        created_at_ms: at,
    };
    w.create_subscription(&s, subscription(&s, 1, at), 20, Some(replay.clone()))
        .await?;
    assert_eq!(
        w.create_subscription(&s, subscription(&s, 2, at), 20, Some(replay.clone()))
            .await,
        Err(StoreError::InvalidBatch("webhooks.replay_exists"))
    );
    assert!(w.get_subscription(&s, &whk(2)).await?.is_none());
    assert_eq!(w.find_replay(&s, "k").await?, Some(replay));
    assert_eq!(w.find_replay(&scope(2), "k").await?, None);
    let sub = w.get_subscription(&s, &whk(1)).await?.unwrap();
    let e = event("evt_cascade", at);
    w.record_event(
        &s,
        e.clone(),
        vec![new_delivery(whd(9), &sub, &e, None, at)],
        None,
    )
    .await?;
    assert!(w.delete_subscription(&s, &whk(1)).await?);
    assert_eq!(w.delivery(&s, &whd(9)).await?, None);
    assert!(w.claim_deliveries(&s, 10, 60_000).await?.is_empty());
    // The event itself stays: it still shows nobody was notified.
    let events = w
        .execution_events(&s, e.execution_id.as_ref().unwrap())
        .await?;
    assert_eq!(events.len(), 1);
    assert!(events[0].1.is_empty());
    Ok(())
}

/// M1: saves are compare-and-set on the revision that was read, and a
/// health change bumps the revision, so a stale PATCH or rotate cannot
/// restore an old secret or undo an automatic disable.
pub async fn stale_saves_conflict<W: WebhookStore>(w: &W) -> Result<(), StoreError> {
    let at = w.webhook_time_ms().await?;
    let s = scope(1);
    let sub = subscription(&s, 1, at);
    w.create_subscription(&s, sub.clone(), 20, None).await?;
    let read_a = w.get_subscription(&s, &whk(1)).await?.unwrap();
    let read_b = read_a.clone();
    let mut rotated = read_a.clone();
    rotated.rotate(SealedSecret("sealed-new".into()), at);
    w.save_subscription(&s, rotated, None).await?;
    let mut stale = read_b;
    stale.name = "renamed".into();
    assert_eq!(
        w.save_subscription(&s, stale, None).await,
        Err(StoreError::InvalidBatch("webhooks.conflict"))
    );
    let current = w.get_subscription(&s, &whk(1)).await?.unwrap();
    assert_eq!(current.secret, SealedSecret("sealed-new".into()));
    assert!(current.revision > read_a.revision);
    // A recorded failure that changes health invalidates earlier reads too.
    let e = event("evt_health", at);
    w.record_event(
        &s,
        e.clone(),
        vec![new_delivery(whd(1), &current, &e, None, at)],
        None,
    )
    .await?;
    let lease = w.claim_deliveries(&s, 1, 60_000).await?.remove(0);
    w.complete_attempt(
        &s,
        &whd(1),
        lease.generation,
        delivered(&lease.delivery, at, false),
    )
    .await?;
    let mut after_failure = current.clone();
    after_failure.enabled = false;
    assert_eq!(
        w.save_subscription(&s, after_failure, None).await,
        Err(StoreError::InvalidBatch("webhooks.conflict"))
    );
    let fresh = w.get_subscription(&s, &whk(1)).await?.unwrap();
    assert_eq!(fresh.failing_since_ms, Some(at));
    w.save_subscription(&s, fresh, None).await?;
    Ok(())
}

/// C1 retention: finished deliveries older than the cutoff go with their
/// attempts, in bounded batches, per tenant; pending deliveries and the
/// delivery states of human events stay (their attempts go).
pub async fn delivery_log_purge_is_bounded_and_keeps_verdicts<W: WebhookStore>(
    w: &W,
) -> Result<(), StoreError> {
    use runbook_core::webhooks::DELIVERY_LOG_RETENTION_MS;
    let now = w.webhook_time_ms().await?;
    let old = now - DELIVERY_LOG_RETENTION_MS - 86_400_000;
    let cutoff = now - DELIVERY_LOG_RETENTION_MS;
    for s in [scope(1), scope(2)] {
        let n = if s == scope(1) { 1 } else { 2 };
        w.create_subscription(&s, subscription(&s, n, old), 20, None)
            .await?;
    }
    let sub = subscription(&scope(1), 1, old);
    let other = subscription(&scope(2), 2, old);
    let record = async |s: &TenantScope,
                        sub: &Subscription,
                        n: u32,
                        human: bool,
                        at: i64|
           -> Result<(), StoreError> {
        let mut e = event(&format!("evt_{n}"), at);
        e.human = human;
        e.event_type = if human {
            "approval.requested"
        } else {
            "step.completed"
        }
        .into();
        let d = new_delivery(whd(n), sub, &e, None, at);
        assert!(w.record_event(s, e, vec![d], None).await?);
        Ok(())
    };
    // Old finished: two non-human, one human. One recent. One pending
    // (not yet due), and one old delivery in another tenant.
    record(&scope(1), &sub, 1, false, old).await?;
    record(&scope(1), &sub, 2, false, old).await?;
    record(&scope(1), &sub, 3, true, old).await?;
    record(&scope(1), &sub, 5, false, now).await?;
    record(&scope(2), &other, 6, false, old).await?;
    for s in [scope(1), scope(2)] {
        for lease in w.claim_deliveries(&s, 10, 60_000).await? {
            let at = lease.delivery.created_at_ms;
            w.complete_attempt(
                &s,
                &lease.delivery.id,
                lease.generation,
                delivered(&lease.delivery, at, true),
            )
            .await?;
        }
    }
    let mut pending = event("evt_4", old);
    pending.human = false;
    let mut waiting = new_delivery(whd(4), &sub, &pending, None, old);
    waiting.next_attempt_at_ms = Some(now + 3_600_000);
    w.record_event(&scope(1), pending, vec![waiting], None)
        .await?;
    // One bounded batch removes the oldest finished non-human delivery.
    assert!(w.purge_delivery_log(&scope(1), cutoff, 1).await? > 0);
    assert_eq!(w.delivery(&scope(1), &whd(1)).await?, None);
    assert!(w.delivery(&scope(1), &whd(2)).await?.is_some());
    while w.purge_delivery_log(&scope(1), cutoff, 1).await? > 0 {}
    assert_eq!(w.delivery(&scope(1), &whd(2)).await?, None);
    let (human, attempts) = w.delivery(&scope(1), &whd(3)).await?.unwrap();
    assert_eq!(human.state, DeliveryState::Delivered);
    assert!(attempts.is_empty(), "attempt detail is purged");
    let (waiting, _) = w.delivery(&scope(1), &whd(4)).await?.unwrap();
    assert_eq!(waiting.state, DeliveryState::Pending);
    let (recent, attempts) = w.delivery(&scope(1), &whd(5)).await?.unwrap();
    assert_eq!(
        (recent.state, attempts.len()),
        (DeliveryState::Delivered, 1)
    );
    let events = w
        .execution_events(
            &scope(1),
            &"exe_00000000000000000000000001".parse().unwrap(),
        )
        .await?;
    let verdict = events.iter().find(|(e, _)| e.event_id == "evt_3").unwrap();
    assert_eq!(verdict.1, [DeliveryState::Delivered]);
    // Another tenant's log is untouched until its own purge.
    assert_eq!(w.delivery(&scope(2), &whd(6)).await?.unwrap().1.len(), 1);
    assert!(w.purge_delivery_log(&scope(2), cutoff, 10).await? > 0);
    assert_eq!(w.delivery(&scope(2), &whd(6)).await?, None);
    Ok(())
}
