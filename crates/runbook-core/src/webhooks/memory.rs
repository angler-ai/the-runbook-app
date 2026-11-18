//! Synchronous in-memory webhook store under a settable clock, mirroring the
//! Postgres adapter's atomicity, tenant filters and lease fencing.
use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
mod portable;

#[derive(Clone)]
struct LeasedDelivery {
    scope: TenantScope,
    record: DeliveryRecord,
    generation: u64,
    lease_deadline_at_ms: Option<i64>,
}

#[derive(Clone, Default)]
struct Db {
    subscriptions: BTreeMap<WebhookId, Subscription>,
    events: BTreeMap<(TenantScope, String), EventRecord>,
    deliveries: BTreeMap<DeliveryId, LeasedDelivery>,
    attempts: BTreeMap<DeliveryId, Vec<AttemptRecord>>,
    replays: BTreeMap<(TenantScope, String), WebhookReplay>,
}

#[derive(Clone, Default)]
pub struct MemoryWebhooks {
    db: Rc<RefCell<Db>>,
    clock: Rc<Cell<i64>>,
    fail_completion: Rc<Cell<bool>>,
}
impl MemoryWebhooks {
    pub fn new(now_ms: i64) -> Self {
        let store = Self::default();
        store.clock.set(now_ms);
        store
    }
    pub fn set_time(&self, now_ms: i64) {
        self.clock.set(now_ms);
    }
    /// Test hook: the next completion fails as if the worker crashed after
    /// the receiver accepted but before the result was recorded.
    pub fn fail_next_completion(&self) {
        self.fail_completion.set(true);
    }
    fn transaction<T>(
        &self,
        f: impl FnOnce(&mut Db) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut committed = self.db.borrow_mut();
        let mut staged = committed.clone();
        let result = f(&mut staged)?;
        *committed = staged;
        Ok(result)
    }
}

fn put_replay(
    db: &mut Db,
    scope: &TenantScope,
    replay: Option<WebhookReplay>,
) -> Result<(), StoreError> {
    if let Some(replay) = replay {
        let key = (scope.clone(), replay.key.clone());
        if db.replays.contains_key(&key) {
            return Err(StoreError::InvalidBatch("webhooks.replay_exists"));
        }
        db.replays.insert(key, replay);
    }
    Ok(())
}

impl WebhookStore for MemoryWebhooks {
    async fn webhook_time_ms(&self) -> Result<i64, StoreError> {
        Ok(self.clock.get())
    }
    async fn list_subscriptions(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<Subscription>, StoreError> {
        let mut list: Vec<Subscription> = self
            .db
            .borrow()
            .subscriptions
            .values()
            .filter(|s| s.scope == *scope)
            .cloned()
            .collect();
        list.sort_by(|a, b| (a.created_at_ms, &a.id).cmp(&(b.created_at_ms, &b.id)));
        Ok(list)
    }
    async fn get_subscription(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<Option<Subscription>, StoreError> {
        Ok(self
            .db
            .borrow()
            .subscriptions
            .get(id)
            .filter(|s| s.scope == *scope)
            .cloned())
    }
    async fn create_subscription(
        &self,
        scope: &TenantScope,
        subscription: Subscription,
        limit: usize,
        replay: Option<WebhookReplay>,
    ) -> Result<(), StoreError> {
        self.transaction(|db| {
            if subscription.scope != *scope || db.subscriptions.contains_key(&subscription.id) {
                return Err(StoreError::InvalidBatch("webhooks.duplicate"));
            }
            if db
                .subscriptions
                .values()
                .filter(|s| s.scope == *scope)
                .count()
                >= limit
            {
                return Err(StoreError::InvalidBatch("webhooks.limit_reached"));
            }
            put_replay(db, scope, replay)?;
            db.subscriptions
                .insert(subscription.id.clone(), subscription);
            Ok(())
        })
    }
    async fn save_subscription(
        &self,
        scope: &TenantScope,
        subscription: Subscription,
        replay: Option<WebhookReplay>,
    ) -> Result<(), StoreError> {
        self.transaction(|db| {
            let mut subscription = subscription;
            match db.subscriptions.get(&subscription.id) {
                Some(existing) if existing.scope == *scope && subscription.scope == *scope => {
                    if existing.revision != subscription.revision {
                        return Err(StoreError::InvalidBatch("webhooks.conflict"));
                    }
                }
                _ => return Err(StoreError::NotFound),
            }
            put_replay(db, scope, replay)?;
            subscription.revision += 1;
            db.subscriptions
                .insert(subscription.id.clone(), subscription);
            Ok(())
        })
    }
    async fn delete_subscription(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<bool, StoreError> {
        self.transaction(|db| {
            if db.subscriptions.get(id).is_none_or(|s| s.scope != *scope) {
                return Ok(false);
            }
            db.subscriptions.remove(id);
            let gone: Vec<DeliveryId> = db
                .deliveries
                .iter()
                .filter(|(_, d)| d.record.subscription_id == *id)
                .map(|(k, _)| k.clone())
                .collect();
            for delivery in gone {
                db.deliveries.remove(&delivery);
                db.attempts.remove(&delivery);
            }
            Ok(true)
        })
    }
    async fn find_replay(
        &self,
        scope: &TenantScope,
        key: &str,
    ) -> Result<Option<WebhookReplay>, StoreError> {
        let now = self.clock.get();
        Ok(self
            .db
            .borrow()
            .replays
            .get(&(scope.clone(), key.to_owned()))
            .filter(|r| now - r.created_at_ms < crate::start::REPLAY_RETENTION_MS)
            .cloned())
    }
    async fn record_event(
        &self,
        scope: &TenantScope,
        event: EventRecord,
        deliveries: Vec<DeliveryRecord>,
        replay: Option<WebhookReplay>,
    ) -> Result<bool, StoreError> {
        self.transaction(|db| {
            let key = (scope.clone(), event.event_id.clone());
            if db.events.contains_key(&key) {
                return Ok(false);
            }
            put_replay(db, scope, replay)?;
            for delivery in deliveries {
                if delivery.event_id != event.event_id
                    || db
                        .subscriptions
                        .get(&delivery.subscription_id)
                        .is_none_or(|s| s.scope != *scope)
                {
                    return Err(StoreError::InvalidBatch("webhooks.invalid_delivery"));
                }
                let first = db.deliveries.values().any(|d| {
                    d.record.redelivery_of.is_none()
                        && d.record.event_id == delivery.event_id
                        && d.record.subscription_id == delivery.subscription_id
                });
                if first || db.deliveries.contains_key(&delivery.id) {
                    return Err(StoreError::InvalidBatch("webhooks.duplicate"));
                }
                db.deliveries.insert(
                    delivery.id.clone(),
                    LeasedDelivery {
                        scope: scope.clone(),
                        record: delivery,
                        generation: 0,
                        lease_deadline_at_ms: None,
                    },
                );
            }
            db.events.insert(key, event);
            Ok(true)
        })
    }
    async fn add_delivery(
        &self,
        scope: &TenantScope,
        delivery: DeliveryRecord,
        replay: Option<WebhookReplay>,
    ) -> Result<(), StoreError> {
        self.transaction(|db| {
            if !db
                .events
                .contains_key(&(scope.clone(), delivery.event_id.clone()))
                || db
                    .subscriptions
                    .get(&delivery.subscription_id)
                    .is_none_or(|s| s.scope != *scope)
            {
                return Err(StoreError::NotFound);
            }
            if delivery.redelivery_of.is_none() || db.deliveries.contains_key(&delivery.id) {
                return Err(StoreError::InvalidBatch("webhooks.duplicate"));
            }
            put_replay(db, scope, replay)?;
            db.deliveries.insert(
                delivery.id.clone(),
                LeasedDelivery {
                    scope: scope.clone(),
                    record: delivery,
                    generation: 0,
                    lease_deadline_at_ms: None,
                },
            );
            Ok(())
        })
    }
    async fn get_event(
        &self,
        scope: &TenantScope,
        event_id: &str,
    ) -> Result<Option<EventRecord>, StoreError> {
        Ok(self
            .db
            .borrow()
            .events
            .get(&(scope.clone(), event_id.to_owned()))
            .cloned())
    }
    async fn claim_deliveries(
        &self,
        scope: &TenantScope,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<DeliveryLease>, StoreError> {
        if lease_ms == 0 {
            return Err(StoreError::InvalidBatch("queue.invalid_lease"));
        }
        let now = self.clock.get();
        let deadline = now.saturating_add(i64::try_from(lease_ms).unwrap_or(i64::MAX));
        self.transaction(|db| {
            let mut due: Vec<(i64, DeliveryId)> = db
                .deliveries
                .iter()
                .filter(|(_, d)| {
                    d.scope == *scope
                        && d.record.state == DeliveryState::Pending
                        && d.record.next_attempt_at_ms.is_some_and(|t| t <= now)
                        && d.lease_deadline_at_ms.is_none_or(|t| t <= now)
                })
                .map(|(id, d)| (d.record.next_attempt_at_ms.unwrap_or(now), id.clone()))
                .collect();
            due.sort();
            let mut out = vec![];
            for (_, id) in due.into_iter().take(limit) {
                let leased = db.deliveries.get_mut(&id).expect("listed above");
                leased.generation += 1;
                leased.lease_deadline_at_ms = Some(deadline);
                let body = db
                    .events
                    .get(&(scope.clone(), leased.record.event_id.clone()))
                    .map(|e| e.body.clone())
                    .ok_or(StoreError::Backend("webhooks.missing_event"))?;
                out.push(DeliveryLease {
                    delivery: leased.record.clone(),
                    generation: leased.generation,
                    claimed_at_ms: now,
                    body,
                });
            }
            Ok(out)
        })
    }
    async fn complete_attempt(
        &self,
        scope: &TenantScope,
        id: &DeliveryId,
        generation: u64,
        completion: AttemptCompletion,
    ) -> Result<(), StoreError> {
        if self.fail_completion.replace(false) {
            return Err(StoreError::Backend("injected"));
        }
        let now = self.clock.get();
        self.transaction(|db| {
            let leased = db
                .deliveries
                .get_mut(id)
                .filter(|d| d.scope == *scope)
                .ok_or(StoreError::NotFound)?;
            if leased.record.state != DeliveryState::Pending
                || leased.generation != generation
                || leased.lease_deadline_at_ms.is_none_or(|t| t <= now)
                || completion.attempt.number != leased.record.attempts + 1
            {
                return Err(StoreError::InvalidBatch("webhooks.stale_lease"));
            }
            leased.record.attempts = completion.attempt.number;
            leased.record.state = completion.state;
            leased.record.next_attempt_at_ms = completion.next_attempt_at_ms;
            leased.record.failure = completion.failure.clone();
            leased.lease_deadline_at_ms = None;
            let subscription_id = leased.record.subscription_id.clone();
            db.attempts
                .entry(id.clone())
                .or_default()
                .push(completion.attempt.clone());
            if let Some(ok) = completion.health
                && let Some(subscription) = db.subscriptions.get_mut(&subscription_id)
            {
                let before = subscription.clone();
                subscription.record_attempt(
                    ok,
                    completion.attempt.at_ms,
                    completion.attempt.failure_label().as_deref(),
                );
                if *subscription != before {
                    subscription.revision += 1;
                }
            }
            Ok(())
        })
    }
    async fn deliveries(
        &self,
        scope: &TenantScope,
        subscription: &WebhookId,
        limit: usize,
        before: Option<(i64, DeliveryId)>,
    ) -> Result<Vec<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError> {
        let db = self.db.borrow();
        let mut rows: Vec<&LeasedDelivery> = db
            .deliveries
            .values()
            .filter(|d| d.scope == *scope && d.record.subscription_id == *subscription)
            .filter(|d| {
                before
                    .as_ref()
                    .is_none_or(|(at, id)| (d.record.created_at_ms, &d.record.id) < (*at, id))
            })
            .collect();
        rows.sort_by(|a, b| {
            (b.record.created_at_ms, &b.record.id).cmp(&(a.record.created_at_ms, &a.record.id))
        });
        Ok(rows
            .into_iter()
            .take(limit)
            .map(|d| {
                (
                    d.record.clone(),
                    db.attempts.get(&d.record.id).cloned().unwrap_or_default(),
                )
            })
            .collect())
    }
    async fn delivery(
        &self,
        scope: &TenantScope,
        id: &DeliveryId,
    ) -> Result<Option<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError> {
        let db = self.db.borrow();
        Ok(db
            .deliveries
            .get(id)
            .filter(|d| d.scope == *scope)
            .map(|d| {
                (
                    d.record.clone(),
                    db.attempts.get(id).cloned().unwrap_or_default(),
                )
            }))
    }
    async fn execution_events(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<Vec<(EventRecord, Vec<DeliveryState>)>, StoreError> {
        let db = self.db.borrow();
        let mut events: Vec<&EventRecord> = db
            .events
            .iter()
            .filter(|((s, _), e)| s == scope && e.execution_id.as_ref() == Some(execution))
            .map(|(_, e)| e)
            .collect();
        events.sort_by(|a, b| (a.created_at_ms, &a.event_id).cmp(&(b.created_at_ms, &b.event_id)));
        Ok(events
            .into_iter()
            .map(|e| {
                let states = db
                    .deliveries
                    .values()
                    .filter(|d| d.scope == *scope && d.record.event_id == e.event_id)
                    .map(|d| d.record.state)
                    .collect();
                (e.clone(), states)
            })
            .collect())
    }
    async fn purge_delivery_log(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> Result<usize, StoreError> {
        self.transaction(|db| {
            let human = |db: &Db, event_id: &str| {
                db.events
                    .get(&(scope.clone(), event_id.to_owned()))
                    .is_some_and(|e| e.human)
            };
            let mut old: Vec<(i64, DeliveryId)> = db
                .deliveries
                .values()
                .filter(|d| {
                    d.scope == *scope
                        && d.record.state != DeliveryState::Pending
                        && d.record.created_at_ms < before_ms
                        && !human(db, &d.record.event_id)
                })
                .map(|d| (d.record.created_at_ms, d.record.id.clone()))
                .collect();
            old.sort();
            old.truncate(limit);
            let mut removed = 0;
            for (_, id) in old {
                db.deliveries.remove(&id);
                removed += 1 + db.attempts.remove(&id).map_or(0, |a| a.len());
            }
            let mut attempts: Vec<(i64, DeliveryId, u32)> = db
                .attempts
                .iter()
                .filter(|(id, _)| db.deliveries.get(*id).is_some_and(|d| d.scope == *scope))
                .flat_map(|(id, list)| {
                    list.iter()
                        .filter(|a| a.at_ms < before_ms)
                        .map(|a| (a.at_ms, id.clone(), a.number))
                })
                .collect();
            attempts.sort();
            attempts.truncate(limit);
            for (_, id, number) in attempts {
                if let Some(list) = db.attempts.get_mut(&id) {
                    list.retain(|a| a.number != number);
                    removed += 1;
                }
            }
            Ok(removed)
        })
    }
}
