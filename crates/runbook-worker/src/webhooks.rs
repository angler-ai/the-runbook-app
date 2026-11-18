//! CF11: webhooks on Cloudflare after the delivery log left D1 (the user's
//! decision, superseding D56's placement). [`CfWebhooks`] is the
//! `WebhookStore`:
//! - D1 (`runbook_store_d1::D1Webhooks`): subscriptions, the tenant's event
//!   records, replay rows and subscription health;
//! - one WebhookDO per subscription (`runbook_store_do::webhook`): its
//!   deliveries, attempts and retry state, and a copy of each event it
//!   delivers, reached over the same transports as the run objects
//!   ([`WebhookRequest`]).
//!
//! A fan-out records the event in D1 first (once, never for a deleted run),
//! then each subscription's deliveries in its object. Delivery IDs derive
//! from (event, subscription), so a retried fan-out writes the same rows
//! and the objects ignore the ones they hold. A completion writes the
//! attempt in the object, then the subscription's health in D1. Delivery
//! IDs name no subscription, so the store remembers which object answered
//! for each delivery it saw and asks the tenant's objects otherwise (a
//! project has at most a few dozen subscriptions).
use crate::protocol::{Reply, Transport, decode_reply, encode_reply};
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    webhooks::{
        AttemptCompletion, AttemptRecord, DeliveryLease, DeliveryRecord, EventRecord, Subscription,
        WebhookReplay, WebhookStore,
    },
};
use runbook_model::{DeliveryId, DeliveryState, ExecutionId, WebhookId};
use runbook_store_d1::{D1, D1Webhooks};
use runbook_store_do::{DoSql, webhook::WebhookLog};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{cell::RefCell, collections::BTreeMap, future::Future, rc::Rc};

/// One operation on a subscription's delivery log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookOp {
    Record {
        event: EventRecord,
        deliveries: Vec<DeliveryRecord>,
    },
    Add {
        delivery: DeliveryRecord,
    },
    Claim {
        now_ms: i64,
        limit: usize,
        lease_ms: u64,
    },
    Complete {
        id: DeliveryId,
        generation: u64,
        completion: AttemptCompletion,
        now_ms: i64,
    },
    Deliveries {
        limit: usize,
        before: Option<(i64, DeliveryId)>,
    },
    Delivery {
        id: DeliveryId,
    },
    States {
        events: Vec<String>,
    },
    Purge {
        before_ms: i64,
        limit: usize,
    },
    PurgeAll,
    NextDue,
    ForgetExecution {
        execution_id: ExecutionId,
    },
}

/// One request to a subscription's WebhookDO.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebhookRequest {
    pub scope: TenantScope,
    pub subscription: WebhookId,
    pub op: WebhookOp,
}

fn to_value<T: Serialize>(value: T) -> Reply {
    serde_json::to_value(value).map_err(|_| StoreError::Backend("webhooks.encode"))
}

/// Answer `request` over the object's storage.
pub fn serve_webhook<B: DoSql>(sql: B, request: WebhookRequest) -> Reply {
    let log = WebhookLog::open(sql, request.scope, request.subscription)?;
    match request.op {
        WebhookOp::Record { event, deliveries } => to_value(log.record(&event, &deliveries)?),
        WebhookOp::Add { delivery } => to_value(log.add(&delivery)?),
        WebhookOp::Claim {
            now_ms,
            limit,
            lease_ms,
        } => to_value(log.claim(now_ms, limit, lease_ms)?),
        WebhookOp::Complete {
            id,
            generation,
            completion,
            now_ms,
        } => to_value(log.complete(&id, generation, &completion, now_ms)?),
        WebhookOp::Deliveries { limit, before } => to_value(log.deliveries(limit, before)?),
        WebhookOp::Delivery { id } => to_value(log.delivery(&id)?),
        WebhookOp::States { events } => to_value(log.states(&events)?),
        WebhookOp::Purge { before_ms, limit } => to_value(log.purge(before_ms, limit)?),
        WebhookOp::PurgeAll => to_value(log.purge_all()?),
        WebhookOp::NextDue => to_value(log.next_due()?),
        WebhookOp::ForgetExecution { execution_id } => {
            to_value(log.forget_execution(execution_id.as_str())?)
        }
    }
}
pub fn encode_webhook_reply(reply: &Reply) -> serde_json::Value {
    encode_reply(reply)
}
pub fn decode_webhook_reply(wire: serde_json::Value) -> Reply {
    decode_reply(wire)
}

/// How a host reaches the subscriptions' objects.
pub trait WebhookObjects: Clone + 'static {
    fn call(&self, request: WebhookRequest) -> impl Future<Output = Reply>;
}

/// The objects over an object-protocol transport (Workers' stubs, the
/// conformance Worker's case objects, the native in-process transport).
#[derive(Clone)]
pub struct ViaTransport<T: Transport>(pub T);
impl<T: Transport> WebhookObjects for ViaTransport<T> {
    async fn call(&self, request: WebhookRequest) -> Reply {
        self.0.call_webhook(request).await
    }
}

/// One SQLite database per subscription in this process (native tests),
/// every request and reply through JSON.
#[derive(Clone)]
pub struct LocalWebhooks<S: DoSql> {
    map: Rc<RefCell<BTreeMap<WebhookId, S>>>,
    make: Rc<dyn Fn() -> S>,
}
impl<S: DoSql> LocalWebhooks<S> {
    pub fn new(make: impl Fn() -> S + 'static) -> Self {
        Self {
            map: Rc::default(),
            make: Rc::new(make),
        }
    }
}
impl<S: DoSql + Default> Default for LocalWebhooks<S> {
    fn default() -> Self {
        Self::new(S::default)
    }
}
impl<S: DoSql> WebhookObjects for LocalWebhooks<S> {
    async fn call(&self, request: WebhookRequest) -> Reply {
        let wire =
            serde_json::to_string(&request).map_err(|_| StoreError::Backend("test.encode"))?;
        let request: WebhookRequest =
            serde_json::from_str(&wire).map_err(|_| StoreError::Backend("test.decode"))?;
        let sql = self
            .map
            .borrow_mut()
            .entry(request.subscription.clone())
            .or_insert_with(|| (self.make)())
            .clone();
        let reply = serve_webhook(sql, request);
        let wire = serde_json::to_string(&encode_webhook_reply(&reply))
            .map_err(|_| StoreError::Backend("test.encode"))?;
        decode_webhook_reply(
            serde_json::from_str(&wire).map_err(|_| StoreError::Backend("test.decode"))?,
        )
    }
}

/// A WebhookDO's own view: its subscription's log in its own storage, the
/// others over `rest` (its alarm never calls itself through a stub).
#[derive(Clone)]
pub struct LocalFirst<S: DoSql, W: WebhookObjects> {
    pub subscription: WebhookId,
    pub sql: S,
    pub rest: W,
}
impl<S: DoSql, W: WebhookObjects> WebhookObjects for LocalFirst<S, W> {
    async fn call(&self, request: WebhookRequest) -> Reply {
        if request.subscription == self.subscription {
            serve_webhook(self.sql.clone(), request)
        } else {
            self.rest.call(request).await
        }
    }
}

/// The webhook store over D1 and the subscriptions' objects.
#[derive(Clone)]
pub struct CfWebhooks<D: D1, W: WebhookObjects> {
    /// CF11: subscriptions, events and replays are on each organization's
    /// shard.
    shards: crate::shards::D1Shards<D>,
    objects: W,
    /// Which subscription's object holds each delivery this store has seen,
    /// by tenant (another tenant never reaches it through this map).
    routes: Rc<RefCell<BTreeMap<(TenantScope, DeliveryId), WebhookId>>>,
    /// A WebhookDO's alarm: claim only its own subscription's deliveries.
    only: Option<WebhookId>,
}

impl<D: D1, W: WebhookObjects> CfWebhooks<D, W> {
    /// Everything on one database.
    pub fn new(db: D, objects: W) -> Self {
        Self::sharded(crate::shards::D1Shards::single(db), objects)
    }
    pub fn sharded(shards: crate::shards::D1Shards<D>, objects: W) -> Self {
        Self {
            shards,
            objects,
            routes: Rc::default(),
            only: None,
        }
    }
    /// The same store, claiming only `subscription`'s deliveries.
    pub fn only(mut self, subscription: WebhookId) -> Self {
        self.only = Some(subscription);
        self
    }
    /// D1's part (subscriptions, events, replays) for `scope`: its
    /// organization's shard.
    pub fn d1(&self, scope: &TenantScope) -> D1Webhooks<D> {
        D1Webhooks::new(self.shards.for_org(&scope.org_id))
    }
    async fn call<R: DeserializeOwned>(
        &self,
        scope: &TenantScope,
        subscription: &WebhookId,
        op: WebhookOp,
    ) -> Result<R, StoreError> {
        let value = self
            .objects
            .call(WebhookRequest {
                scope: scope.clone(),
                subscription: subscription.clone(),
                op,
            })
            .await?;
        serde_json::from_value(value).map_err(|_| StoreError::Backend("webhooks.decode"))
    }
    /// Gate round 2: a redelivery's replay row commits before its Add, and
    /// an Add that failed ambiguously leaves the row. Whoever finds the row
    /// makes sure the delivery it answers for exists: rebuilt from the saved
    /// view, the subscription and the event, and added (idempotent by ID).
    async fn complete_redelivery(
        &self,
        scope: &TenantScope,
        replay: &WebhookReplay,
    ) -> Result<(), StoreError> {
        let Ok(view) =
            serde_json::from_value::<runbook_model::DeliveryView>(replay.response.clone())
        else {
            return Ok(());
        };
        let Some(original) = view.redelivery_of else {
            return Ok(());
        };
        if self.delivery(scope, &view.id).await?.is_some() {
            return Ok(());
        }
        let d1 = self.d1(scope);
        let (Some(subscription), Some(event)) = (
            d1.get_subscription(scope, &view.webhook_id).await?,
            d1.get_event(scope, &view.event_id).await?,
        ) else {
            return Ok(());
        };
        let delivery = runbook_core::webhooks::new_delivery(
            view.id,
            &subscription,
            &event,
            Some(original),
            view.created_at_ms,
        );
        self.remember(scope, &delivery.id, &subscription.id);
        self.call(scope, &subscription.id, WebhookOp::Add { delivery })
            .await
    }
    /// Gate round 2: a call in a per-subscription pass, where a retired
    /// object (its subscription deleted) answers `NotFound`: nothing to do
    /// there, so the pass goes on instead of failing.
    async fn call_live<R: DeserializeOwned + Default>(
        &self,
        scope: &TenantScope,
        subscription: &WebhookId,
        op: WebhookOp,
    ) -> Result<R, StoreError> {
        match self.call(scope, subscription, op).await {
            Err(StoreError::NotFound) => Ok(R::default()),
            other => other,
        }
    }
    fn remember(&self, scope: &TenantScope, id: &DeliveryId, subscription: &WebhookId) {
        self.routes
            .borrow_mut()
            .insert((scope.clone(), id.clone()), subscription.clone());
    }
    /// Which subscription's object holds `id`, asking each of the tenant's
    /// when this store has not seen it.
    async fn route(
        &self,
        scope: &TenantScope,
        id: &DeliveryId,
    ) -> Result<Option<WebhookId>, StoreError> {
        if let Some(subscription) = self.routes.borrow().get(&(scope.clone(), id.clone())) {
            return Ok(Some(subscription.clone()));
        }
        for subscription in self.d1(scope).list_subscriptions(scope).await? {
            let found: Option<(DeliveryRecord, Vec<AttemptRecord>)> = self
                .call_live(
                    scope,
                    &subscription.id,
                    WebhookOp::Delivery { id: id.clone() },
                )
                .await?;
            if found.is_some() {
                self.remember(scope, id, &subscription.id);
                return Ok(Some(subscription.id));
            }
        }
        Ok(None)
    }
    /// A deleted run's deliveries leave every subscription's log (its D1
    /// event records went with the deletion's batch). Returns how many.
    pub async fn forget_execution(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<usize, StoreError> {
        let mut forgotten = 0;
        for subscription in self.d1(scope).list_subscriptions(scope).await? {
            let n: usize = self
                .call_live(
                    scope,
                    &subscription.id,
                    WebhookOp::ForgetExecution {
                        execution_id: execution.clone(),
                    },
                )
                .await?;
            forgotten += n;
        }
        Ok(forgotten)
    }
    /// When a subscription's object next has work (its alarm).
    pub async fn next_due(
        &self,
        scope: &TenantScope,
        subscription: &WebhookId,
    ) -> Result<Option<i64>, StoreError> {
        self.call(scope, subscription, WebhookOp::NextDue).await
    }
}

impl<D: D1, W: WebhookObjects> WebhookStore for CfWebhooks<D, W> {
    async fn webhook_time_ms(&self) -> Result<i64, StoreError> {
        Ok(self.shards.control().now_ms())
    }
    async fn list_subscriptions(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<Subscription>, StoreError> {
        self.d1(scope).list_subscriptions(scope).await
    }
    async fn get_subscription(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<Option<Subscription>, StoreError> {
        self.d1(scope).get_subscription(scope, id).await
    }
    async fn create_subscription(
        &self,
        scope: &TenantScope,
        subscription: Subscription,
        limit: usize,
        replay: Option<WebhookReplay>,
    ) -> Result<(), StoreError> {
        self.d1(scope)
            .create_subscription(scope, subscription, limit, replay)
            .await
    }
    async fn save_subscription(
        &self,
        scope: &TenantScope,
        subscription: Subscription,
        replay: Option<WebhookReplay>,
    ) -> Result<(), StoreError> {
        self.d1(scope)
            .save_subscription(scope, subscription, replay)
            .await
    }
    /// D1 first (the commit point); then the object is emptied. An object
    /// left behind by a failure is unreachable (no subscription names it).
    async fn delete_subscription(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<bool, StoreError> {
        let deleted = self.d1(scope).delete_subscription(scope, id).await?;
        if deleted {
            let _: Result<(), StoreError> = self.call(scope, id, WebhookOp::PurgeAll).await;
        }
        Ok(deleted)
    }
    async fn find_replay(
        &self,
        scope: &TenantScope,
        key: &str,
    ) -> Result<Option<WebhookReplay>, StoreError> {
        let found = self.d1(scope).find_replay(scope, key).await?;
        if let Some(replay) = &found {
            self.complete_redelivery(scope, replay).await?;
        }
        Ok(found)
    }
    async fn record_event(
        &self,
        scope: &TenantScope,
        event: EventRecord,
        deliveries: Vec<DeliveryRecord>,
        replay: Option<WebhookReplay>,
    ) -> Result<bool, StoreError> {
        if deliveries.iter().any(|d| d.event_id != event.event_id) {
            return Err(StoreError::InvalidBatch("webhooks.invalid_delivery"));
        }
        let subscriptions = self.d1(scope).list_subscriptions(scope).await?;
        if deliveries
            .iter()
            .any(|d| !subscriptions.iter().any(|s| s.id == d.subscription_id))
        {
            return Err(StoreError::InvalidBatch("webhooks.invalid_delivery"));
        }
        let recorded = self
            .d1(scope)
            .record_event_row(scope, &event, replay.as_ref())
            .await?;
        if !recorded
            && !self
                .d1(scope)
                .event_recorded(scope, &event.event_id)
                .await?
        {
            // Not recorded because the run was deleted: nothing to deliver.
            return Ok(false);
        }
        let mut by_subscription: BTreeMap<WebhookId, Vec<DeliveryRecord>> = BTreeMap::new();
        for delivery in deliveries {
            by_subscription
                .entry(delivery.subscription_id.clone())
                .or_default()
                .push(delivery);
        }
        for (subscription, deliveries) in by_subscription {
            for delivery in &deliveries {
                self.remember(scope, &delivery.id, &subscription);
            }
            let _: bool = self
                .call_live(
                    scope,
                    &subscription,
                    WebhookOp::Record {
                        event: event.clone(),
                        deliveries,
                    },
                )
                .await?;
        }
        Ok(recorded)
    }
    async fn add_delivery(
        &self,
        scope: &TenantScope,
        delivery: DeliveryRecord,
        replay: Option<WebhookReplay>,
    ) -> Result<(), StoreError> {
        if delivery.redelivery_of.is_none() {
            return Err(StoreError::InvalidBatch("webhooks.duplicate"));
        }
        if self
            .d1(scope)
            .get_subscription(scope, &delivery.subscription_id)
            .await?
            .is_none()
            || !self
                .d1(scope)
                .event_recorded(scope, &delivery.event_id)
                .await?
        {
            return Err(StoreError::NotFound);
        }
        // Gate fix (low): the replay row is the commit point, written first
        // and guarded (a taken key is `replay_exists`); the object's Add is
        // idempotent by delivery ID; an Add that fails takes the replay row
        // back, so a retry starts clean instead of finding a replay for a
        // delivery that was never added.
        if let Some(replay) = &replay {
            if self
                .d1(scope)
                .find_replay(scope, &replay.key)
                .await?
                .is_some()
            {
                return Err(StoreError::InvalidBatch("webhooks.replay_exists"));
            }
            match self.d1(scope).write_replay(scope, replay).await {
                Ok(()) => {}
                Err(StoreError::InvalidBatch("webhooks.duplicate")) => {
                    return Err(StoreError::InvalidBatch("webhooks.replay_exists"));
                }
                Err(error) => return Err(error),
            }
        }
        let subscription = delivery.subscription_id.clone();
        self.remember(scope, &delivery.id, &subscription);
        let added: Result<(), StoreError> = self
            .call(scope, &subscription, WebhookOp::Add { delivery })
            .await;
        // Gate round 2: only a definite refusal takes the replay row back;
        // after an ambiguous failure (the Add may have happened) it stays,
        // and the retry that finds it completes the delivery
        // (`complete_redelivery`).
        if let (Err(StoreError::InvalidBatch(_) | StoreError::NotFound), Some(replay)) =
            (&added, &replay)
        {
            let _ = self.d1(scope).delete_replay(scope, &replay.key).await;
        }
        added
    }
    async fn get_event(
        &self,
        scope: &TenantScope,
        event_id: &str,
    ) -> Result<Option<EventRecord>, StoreError> {
        self.d1(scope).get_event(scope, event_id).await
    }
    /// The tenant's due deliveries, subscription by subscription, until
    /// `limit`.
    async fn claim_deliveries(
        &self,
        scope: &TenantScope,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<DeliveryLease>, StoreError> {
        if lease_ms == 0 {
            return Err(StoreError::InvalidBatch("queue.invalid_lease"));
        }
        let now = self.shards.control().now_ms();
        let mut out = vec![];
        for subscription in self.d1(scope).list_subscriptions(scope).await? {
            if out.len() >= limit {
                break;
            }
            if self
                .only
                .as_ref()
                .is_some_and(|only| only != &subscription.id)
            {
                continue;
            }
            let leases: Vec<DeliveryLease> = self
                .call_live(
                    scope,
                    &subscription.id,
                    WebhookOp::Claim {
                        now_ms: now,
                        limit: limit - out.len(),
                        lease_ms,
                    },
                )
                .await?;
            for lease in &leases {
                self.remember(scope, &lease.delivery.id, &subscription.id);
            }
            out.extend(leases);
        }
        out.sort_by(|a, b| {
            (a.delivery.next_attempt_at_ms, &a.delivery.id)
                .cmp(&(b.delivery.next_attempt_at_ms, &b.delivery.id))
        });
        Ok(out)
    }
    async fn complete_attempt(
        &self,
        scope: &TenantScope,
        id: &DeliveryId,
        generation: u64,
        completion: AttemptCompletion,
    ) -> Result<(), StoreError> {
        let subscription = self.route(scope, id).await?.ok_or(StoreError::NotFound)?;
        let health = completion.health;
        let at_ms = completion.attempt.at_ms;
        let failure = completion.attempt.failure_label();
        let _: DeliveryRecord = self
            .call(
                scope,
                &subscription,
                WebhookOp::Complete {
                    id: id.clone(),
                    generation,
                    completion,
                    now_ms: self.shards.control().now_ms(),
                },
            )
            .await?;
        if let Some(ok) = health {
            self.d1(scope)
                .record_health(scope, &subscription, ok, at_ms, failure.as_deref())
                .await?;
        }
        Ok(())
    }
    async fn deliveries(
        &self,
        scope: &TenantScope,
        subscription: &WebhookId,
        limit: usize,
        before: Option<(i64, DeliveryId)>,
    ) -> Result<Vec<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError> {
        if self
            .d1(scope)
            .get_subscription(scope, subscription)
            .await?
            .is_none()
        {
            return Ok(vec![]);
        }
        let page: Vec<(DeliveryRecord, Vec<AttemptRecord>)> = self
            .call(scope, subscription, WebhookOp::Deliveries { limit, before })
            .await?;
        for (delivery, _) in &page {
            self.remember(scope, &delivery.id, subscription);
        }
        Ok(page)
    }
    async fn delivery(
        &self,
        scope: &TenantScope,
        id: &DeliveryId,
    ) -> Result<Option<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError> {
        let Some(subscription) = self.route(scope, id).await? else {
            return Ok(None);
        };
        match self
            .call(scope, &subscription, WebhookOp::Delivery { id: id.clone() })
            .await
        {
            // A deleted subscription's retired object (gate fix D3).
            Err(StoreError::NotFound) => Ok(None),
            other => other,
        }
    }
    async fn execution_events(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<Vec<(EventRecord, Vec<DeliveryState>)>, StoreError> {
        let events = self
            .d1(scope)
            .execution_event_records(scope, execution)
            .await?;
        if events.is_empty() {
            return Ok(vec![]);
        }
        let ids: Vec<String> = events.iter().map(|e| e.event_id.clone()).collect();
        let mut states: BTreeMap<String, Vec<DeliveryState>> = BTreeMap::new();
        for subscription in self.d1(scope).list_subscriptions(scope).await? {
            let found: BTreeMap<String, Vec<DeliveryState>> = self
                .call_live(
                    scope,
                    &subscription.id,
                    WebhookOp::States {
                        events: ids.clone(),
                    },
                )
                .await?;
            for (event, mut more) in found {
                states.entry(event).or_default().append(&mut more);
            }
        }
        Ok(events
            .into_iter()
            .map(|event| {
                let mine = states.remove(&event.event_id).unwrap_or_default();
                (event, mine)
            })
            .collect())
    }
    async fn purge_delivery_log(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> Result<usize, StoreError> {
        let mut removed = 0;
        for subscription in self.d1(scope).list_subscriptions(scope).await? {
            let n: usize = self
                .call_live(
                    scope,
                    &subscription.id,
                    WebhookOp::Purge { before_ms, limit },
                )
                .await?;
            removed += n;
        }
        removed += self.d1(scope).purge_events(scope, before_ms, limit).await?;
        Ok(removed)
    }
}

/// Gate fix D3: one WebhookDO alarm pass for subscription `id`. An object
/// whose subscription D1 no longer has (deleted, the purge that should have
/// followed lost) purges and retires itself: `Ok("retired")`. Otherwise its
/// due deliveries are attempted. `Err` is a failed pass (D1 unreachable, no
/// sealer key, a store error), which the caller backs off from.
pub async fn alarm<D, W, T, K>(
    webhooks: &CfWebhooks<D, W>,
    transport: &T,
    sealer: Option<&K>,
    scope: &TenantScope,
    id: &WebhookId,
) -> Result<String, String>
where
    D: D1,
    W: WebhookObjects,
    T: runbook_core::webhooks::WebhookTransport,
    K: runbook_core::webhooks::SecretSealer,
{
    let code = |e: StoreError| match e {
        StoreError::NotFound => "not_found".to_owned(),
        StoreError::InvalidBatch(c) | StoreError::Backend(c) => c.to_owned(),
    };
    match webhooks.d1(scope).get_subscription(scope, id).await {
        Ok(Some(_)) => {}
        // Gate round 2: retire only on positive evidence (D1's deletion
        // marker); a subscription D1 merely does not find is a failed pass.
        Ok(None) => {
            if !webhooks
                .d1(scope)
                .subscription_deleted(scope, id)
                .await
                .map_err(code)?
            {
                return Err("webhooks.subscription_unknown".to_owned());
            }
            webhooks
                .call::<()>(scope, id, WebhookOp::PurgeAll)
                .await
                .map_err(code)?;
            return Ok("retired".to_owned());
        }
        Err(error) => return Err(code(error)),
    }
    let sealer = sealer.ok_or_else(|| "webhooks.unconfigured".to_owned())?;
    runbook_core::webhooks::deliver_due(
        webhooks,
        transport,
        sealer,
        scope,
        runbook_core::webhooks::DeliverySettings::default(),
    )
    .await
    .map(|stats| format!("delivered {}, retried {}", stats.delivered, stats.retried))
    .map_err(code)
}
