//! CF4 notification delivery over the real DO store (native SQLite) and
//! core's in-memory webhooks: the producer sends then acks, a failed send
//! acks nothing and backs off, and the consumer is core's signed,
//! retried, logged delivery.
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    webhooks::{
        self, DeliverySettings, OutgoingRequest, SealedSecret, SecretSealer, Subscription,
        TransportResult, WebhookStore, WebhookTransport, memory::MemoryWebhooks,
    },
};
use runbook_model::{DeliveryState, ExecutionId};
use runbook_store_do::{DoStore, OutboxStream};
use runbook_worker::{
    delivery::{FANOUT_RETRY_SECONDS, NotificationMessage, NotificationQueue, consume, publish},
    spike,
};
use runbook_worker_conformance::native::{NativeSql, run};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

const T0: i64 = 1_700_000_000_000;

fn started() -> (DoStore<NativeSql>, NativeSql) {
    let id: ExecutionId = "exe_00000000000000000000000001".parse().unwrap();
    let sql = NativeSql::new(T0);
    let store = DoStore::open(sql.clone(), &id, spike::scope()).unwrap();
    run(spike::start_with(&store, &id, T0)).unwrap();
    (store, sql)
}

#[derive(Clone, Default)]
struct Queue {
    sent: Rc<RefCell<Vec<NotificationMessage>>>,
    fail: Rc<Cell<bool>>,
}
impl NotificationQueue for Queue {
    async fn send_batch(&self, messages: Vec<NotificationMessage>) -> Result<(), StoreError> {
        if self.fail.get() {
            return Err(StoreError::Backend("queue.unavailable"));
        }
        self.sent.borrow_mut().extend(messages);
        Ok(())
    }
}

#[test]
fn publish_sends_every_intent_then_acks_them() {
    let (store, _) = started();
    let pending = store.pending_notifications(None, 100).unwrap();
    assert!(!pending.is_empty(), "the start wrote an intent");
    let queue = Queue::default();
    assert_eq!(run(publish(&store, &queue)).unwrap(), pending.len());
    let sent = queue.sent.borrow();
    assert_eq!(
        sent.iter().map(|m| &m.intent).collect::<Vec<_>>(),
        pending.iter().collect::<Vec<_>>()
    );
    assert!(sent.iter().all(|m| m.scope == spike::scope()));
    assert!(sent.iter().all(|m| m.meta.id == *store.name()));
    assert!(sent.iter().all(|m| m.meta.url == m.intent.link));
    assert_eq!(
        sent[0].meta.title, "CF1 spike",
        "the run's title from its derived row"
    );
    assert!(store.pending_notifications(None, 100).unwrap().is_empty());
    assert_eq!(run(publish(&store, &queue)).unwrap(), 0, "nothing left");
}

#[test]
fn a_failed_send_acks_nothing_and_backs_the_stream_off() {
    let (store, sql) = started();
    let pending = store.pending_notifications(None, 100).unwrap();
    let queue = Queue::default();
    queue.fail.set(true);
    sql.set_time(T0 + 20_000);
    assert_eq!(
        run(publish(&store, &queue)),
        Err(StoreError::Backend("queue.unavailable"))
    );
    assert_eq!(store.pending_notifications(None, 100).unwrap(), pending);
    assert_eq!(
        store.outbox_retry_at(OutboxStream::Notification).unwrap(),
        Some(T0 + 20_000 + 10_000),
        "the alarm retries after the first backoff step"
    );
    queue.fail.set(false);
    assert_eq!(run(publish(&store, &queue)).unwrap(), pending.len());
    assert!(store.pending_notifications(None, 100).unwrap().is_empty());
}

/// A reversible test sealer (production seals with the host's key).
struct Sealer;
impl SecretSealer for Sealer {
    fn seal(&self, context: &str, secret: &str) -> Result<SealedSecret, StoreError> {
        Ok(SealedSecret(format!("test:{context}:{secret}")))
    }
    fn open(&self, context: &str, sealed: &SealedSecret) -> Result<String, StoreError> {
        sealed
            .0
            .strip_prefix(&format!("test:{context}:"))
            .map(str::to_owned)
            .ok_or(StoreError::Backend("sealer.context"))
    }
}

/// Records requests; answers from a script (default 200).
#[derive(Clone, Default)]
struct Receiver {
    seen: Rc<RefCell<Vec<OutgoingRequest>>>,
    script: Rc<RefCell<VecDeque<TransportResult>>>,
}
impl WebhookTransport for Receiver {
    async fn post(&self, request: OutgoingRequest) -> TransportResult {
        self.seen.borrow_mut().push(request);
        self.script
            .borrow_mut()
            .pop_front()
            .unwrap_or(TransportResult::Response {
                status: 200,
                latency_ms: 3,
                snippet: "ok".into(),
            })
    }
}

fn subscribe(webhooks: &MemoryWebhooks, scope: &TenantScope) {
    let id: runbook_model::WebhookId = format!("whk_{:026}", 1).parse().unwrap();
    let secret = webhooks::encode_webhook_secret([9; 32]);
    run(webhooks.create_subscription(
        scope,
        Subscription {
            id: id.clone(),
            scope: scope.clone(),
            name: "hook".into(),
            url: "https://hooks.example/runbook".into(),
            events: vec![webhooks::ALL_EVENTS.into()],
            runbooks: vec![],
            tags: vec![],
            enabled: true,
            secret: Sealer.seal(id.as_str(), &secret).unwrap(),
            previous_secret: None,
            created_at_ms: T0,
            updated_at_ms: T0,
            secret_rotated_at_ms: T0,
            failing_since_ms: None,
            last_failure_at_ms: None,
            disabled_reason: None,
            disabled_at_ms: None,
            revision: 0,
            notice: None,
        },
        webhooks::SUBSCRIPTION_LIMIT,
        None,
    ))
    .unwrap();
}

#[test]
fn the_consumer_fans_out_once_signs_and_retries_like_native() {
    let (store, _) = started();
    let queue = Queue::default();
    run(publish(&store, &queue)).unwrap();
    let message = queue.sent.borrow()[0].clone();
    let webhooks = MemoryWebhooks::new(T0);
    subscribe(&webhooks, &message.scope);
    let receiver = Receiver::default();
    receiver
        .script
        .borrow_mut()
        .push_back(TransportResult::Response {
            status: 503,
            latency_ms: 3,
            snippet: "busy".into(),
        });
    let settings = DeliverySettings::default();
    let stats = run(consume(
        &webhooks,
        &receiver,
        &Sealer,
        message.clone(),
        settings,
    ))
    .unwrap();
    assert_eq!((stats.claimed, stats.retried), (1, 1), "503 retries");
    {
        let seen = receiver.seen.borrow();
        let request = &seen[0];
        let header = |name: &str| {
            request
                .headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            header("Runbook-Event-Id"),
            Some(message.intent.event_id.clone())
        );
        assert_eq!(
            header("Runbook-Event-Type"),
            Some(message.intent.kind.clone())
        );
        assert!(header("Runbook-Signature").is_some_and(|s| s.starts_with("v1=")));
        let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["id"], message.intent.event_id.as_str());
        assert_eq!(body["execution"]["id"], message.meta.id.as_str());
        assert_eq!(body["execution"]["title"], "CF1 spike");
    }
    let whk: runbook_model::WebhookId = format!("whk_{:026}", 1).parse().unwrap();
    let log = run(webhooks.deliveries(&message.scope, &whk, 10, None)).unwrap();
    assert_eq!(log.len(), 1);
    let (delivery, attempts) = &log[0];
    assert_eq!(delivery.state, DeliveryState::Pending);
    assert_eq!(attempts.len(), 1, "the attempt is logged");
    assert!(delivery.next_attempt_at_ms.is_some_and(|at| at > T0));
    // The queue redelivers the same message: nothing is recorded twice and
    // nothing is attempted before the backoff.
    let again = run(consume(
        &webhooks,
        &receiver,
        &Sealer,
        message.clone(),
        settings,
    ))
    .unwrap();
    assert_eq!(again.claimed, 0);
    assert_eq!(
        run(webhooks.deliveries(&message.scope, &whk, 10, None))
            .unwrap()
            .len(),
        1
    );
    // After the backoff the retry delivers.
    webhooks.set_time(delivery.next_attempt_at_ms.unwrap());
    let later = run(webhooks::deliver_due(
        &webhooks,
        &receiver,
        &Sealer,
        &message.scope,
        settings,
    ))
    .unwrap();
    assert_eq!(later.delivered, 1);
    assert_eq!(receiver.seen.borrow().len(), 2);
}

#[test]
fn a_failed_fan_out_retries_after_the_native_lease() {
    assert_eq!(
        u64::from(FANOUT_RETRY_SECONDS) * 1000,
        DeliverySettings::default().lease_ms
    );
}

struct Redelivery {
    webhooks: MemoryWebhooks,
    receiver: Receiver,
    tenants: Vec<TenantScope>,
}
impl runbook_worker::cron::DeliveryHost for Redelivery {
    type Webhooks = MemoryWebhooks;
    type Transport = Receiver;
    type Sealer = Sealer;
    async fn tenants(&self) -> Result<Vec<TenantScope>, StoreError> {
        Ok(self.tenants.clone())
    }
    fn webhooks(&self) -> &MemoryWebhooks {
        &self.webhooks
    }
    fn transport(&self) -> &Receiver {
        &self.receiver
    }
    fn sealer(&self) -> &Sealer {
        &Sealer
    }
}

/// The cron's redelivery pass sends what waited out its backoff.
#[test]
fn the_cron_redelivers_deliveries_once_their_backoff_ends() {
    let (store, _) = started();
    let queue = Queue::default();
    run(publish(&store, &queue)).unwrap();
    let message = queue.sent.borrow()[0].clone();
    let webhooks = MemoryWebhooks::new(T0);
    subscribe(&webhooks, &message.scope);
    let receiver = Receiver::default();
    receiver
        .script
        .borrow_mut()
        .push_back(TransportResult::Timeout { latency_ms: 10_000 });
    let first = run(consume(
        &webhooks,
        &receiver,
        &Sealer,
        message.clone(),
        DeliverySettings::default(),
    ))
    .unwrap();
    assert_eq!(first.retried, 1, "a timeout retries");
    let host = Redelivery {
        webhooks: webhooks.clone(),
        receiver: receiver.clone(),
        tenants: vec![message.scope.clone()],
    };
    let early = run(runbook_worker::cron::redelivery_pass(&host)).unwrap();
    assert_eq!(early.claimed, 0, "not before the backoff");
    webhooks.set_time(T0 + 10_000 + 30_000);
    let due = run(runbook_worker::cron::redelivery_pass(&host)).unwrap();
    assert_eq!((due.claimed, due.delivered), (1, 1));
}
