//! C1 WebhookStore on PostgreSQL. Each call is one transaction; every query
//! filters by organization and project. Claims lock due rows with
//! `FOR UPDATE SKIP LOCKED` and bump a generation that fences completion,
//! all on the database clock.
use crate::{
    DatabaseConfig,
    execution::{READ_COMMITTED, finish},
    pool::Pool,
    rows::{connection, db, decode, encode, int, is_unique_violation, label, uint},
};
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    webhooks::{
        AttemptCompletion, AttemptRecord, DeliveryLease, DeliveryRecord, EventRecord, Subscription,
        WebhookReplay, WebhookStore,
    },
};
use runbook_model::{DeliveryId, DeliveryState, ExecutionId, WebhookId};
use std::{cell::Cell, rc::Rc};
use tokio_postgres::Client;

const NOW_MS: &str = "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint";

#[derive(Clone)]
pub struct PostgresWebhooks {
    pool: Rc<Pool>,
    fail_completion: Rc<Cell<bool>>,
}
impl PostgresWebhooks {
    pub fn new(config: DatabaseConfig) -> Self {
        Self {
            pool: Pool::new(config),
            fail_completion: Rc::default(),
        }
    }
    /// Test hook: the next completion fails before writing, as if the
    /// worker crashed after the receiver accepted.
    #[doc(hidden)]
    pub fn fail_next_completion(&self) {
        self.fail_completion.set(true);
    }
    async fn transaction<T>(
        &self,
        body: impl AsyncFnOnce(&Client) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(READ_COMMITTED).await.map_err(db)?;
        let result = body(conn.client()).await;
        finish(&mut conn, result).await
    }
}

async fn now_ms(c: &Client) -> Result<i64, StoreError> {
    c.query_one(NOW_MS, &[]).await.map(|r| r.get(0)).map_err(db)
}
fn tenant(scope: &TenantScope) -> (&str, &str) {
    (scope.org_id.as_str(), scope.project_id.as_str())
}
async fn put_replay(
    c: &Client,
    scope: &TenantScope,
    replay: Option<WebhookReplay>,
) -> Result<(), StoreError> {
    let Some(replay) = replay else { return Ok(()) };
    let (org, project) = tenant(scope);
    // Expired rows give way to a new use of the key (D20 24-hour window).
    c.execute(
        "DELETE FROM runbook.webhook_replay WHERE org_id = $1 AND project_id = $2 \
         AND replay_key = $3 AND created_at_ms <= $4",
        &[
            &org,
            &project,
            &replay.key,
            &(replay.created_at_ms - runbook_core::start::REPLAY_RETENTION_MS),
        ],
    )
    .await
    .map_err(db)?;
    match c
        .execute(
            "INSERT INTO runbook.webhook_replay (org_id, project_id, replay_key, created_at_ms, body) \
             VALUES ($1, $2, $3, $4, $5)",
            &[&org, &project, &replay.key, &replay.created_at_ms, &encode(&replay)?],
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(e) if is_unique_violation(&e) => Err(StoreError::InvalidBatch("webhooks.replay_exists")),
        Err(e) => Err(db(e)),
    }
}
/// Serializes a run's deletion against fan-out of its events: deletion
/// takes this exclusively, recording an event of that run takes it shared.
const RUN_EVENTS_LOCK: &str = "hashtextextended('webhook_run:' || $1, 0)";

/// F31: a deleted run's recorded events, deliveries and attempts (envelopes
/// carry titles and recipient IDs), inside the deletion's transaction.
pub(crate) async fn delete_run_rows(
    c: &Client,
    scope: &TenantScope,
    id: &ExecutionId,
) -> Result<(), StoreError> {
    let (org, project) = tenant(scope);
    c.execute(
        &format!("SELECT pg_advisory_xact_lock({RUN_EVENTS_LOCK})"),
        &[&id.as_str()],
    )
    .await
    .map_err(db)?;
    // Attempts go with their deliveries (ON DELETE CASCADE).
    for sql in [
        "DELETE FROM runbook.webhook_delivery d USING runbook.webhook_event e \
         WHERE e.org_id = $1 AND e.project_id = $2 AND e.execution_id = $3 \
         AND d.org_id = e.org_id AND d.project_id = e.project_id AND d.event_id = e.event_id",
        "DELETE FROM runbook.webhook_event \
         WHERE org_id = $1 AND project_id = $2 AND execution_id = $3",
    ] {
        c.execute(sql, &[&org, &project, &id.as_str()])
            .await
            .map_err(db)?;
    }
    Ok(())
}
async fn lock_subscription(
    c: &Client,
    scope: &TenantScope,
    id: &WebhookId,
) -> Result<Option<Subscription>, StoreError> {
    let (org, project) = tenant(scope);
    c.query_opt(
        "SELECT body FROM runbook.webhook_subscription \
         WHERE subscription_id = $1 AND org_id = $2 AND project_id = $3 FOR UPDATE",
        &[&id.as_str(), &org, &project],
    )
    .await
    .map_err(db)?
    .map(|r| decode(r.get(0)))
    .transpose()
}
async fn write_subscription(c: &Client, subscription: &Subscription) -> Result<(), StoreError> {
    c.execute(
        "UPDATE runbook.webhook_subscription SET body = $2 WHERE subscription_id = $1",
        &[&subscription.id.as_str(), &encode(subscription)?],
    )
    .await
    .map_err(db)?;
    Ok(())
}
async fn insert_delivery(
    c: &Client,
    scope: &TenantScope,
    delivery: &DeliveryRecord,
) -> Result<(), StoreError> {
    let (org, project) = tenant(scope);
    c.execute(
        "INSERT INTO runbook.webhook_delivery (delivery_id, org_id, project_id, subscription_id, \
         event_id, redelivery_of, state, next_attempt_at_ms, created_at_ms, body) \
         SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10 FROM runbook.webhook_subscription s \
         WHERE s.subscription_id = $4 AND s.org_id = $2 AND s.project_id = $3",
        &[
            &delivery.id.as_str(),
            &org,
            &project,
            &delivery.subscription_id.as_str(),
            &delivery.event_id,
            &delivery.redelivery_of.as_ref().map(DeliveryId::as_str),
            &label(&delivery.state)?,
            &delivery.next_attempt_at_ms,
            &delivery.created_at_ms,
            &encode(delivery)?,
        ],
    )
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            StoreError::InvalidBatch("webhooks.duplicate")
        } else {
            db(e)
        }
    })
    .and_then(|n| {
        if n == 1 {
            Ok(())
        } else {
            Err(StoreError::InvalidBatch("webhooks.invalid_delivery"))
        }
    })
}
async fn attempts(c: &Client, id: &str) -> Result<Vec<AttemptRecord>, StoreError> {
    c.query(
        "SELECT body FROM runbook.webhook_attempt WHERE delivery_id = $1 ORDER BY number",
        &[&id],
    )
    .await
    .map_err(db)?
    .iter()
    .map(|r| decode(r.get(0)))
    .collect()
}

impl WebhookStore for PostgresWebhooks {
    async fn webhook_time_ms(&self) -> Result<i64, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        now_ms(conn.client()).await
    }
    async fn list_subscriptions(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<Subscription>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        let (org, project) = tenant(scope);
        conn.client()
            .query(
                "SELECT body FROM runbook.webhook_subscription WHERE org_id = $1 AND project_id = $2 \
                 ORDER BY created_at_ms, subscription_id",
                &[&org, &project],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|r| decode(r.get(0)))
            .collect()
    }
    async fn get_subscription(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<Option<Subscription>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        let (org, project) = tenant(scope);
        conn.client()
            .query_opt(
                "SELECT body FROM runbook.webhook_subscription \
                 WHERE subscription_id = $1 AND org_id = $2 AND project_id = $3",
                &[&id.as_str(), &org, &project],
            )
            .await
            .map_err(db)?
            .map(|r| decode(r.get(0)))
            .transpose()
    }
    async fn create_subscription(
        &self,
        scope: &TenantScope,
        subscription: Subscription,
        limit: usize,
        replay: Option<WebhookReplay>,
    ) -> Result<(), StoreError> {
        if subscription.scope != *scope {
            return Err(StoreError::InvalidBatch("webhooks.duplicate"));
        }
        self.transaction(async |c| {
            let (org, project) = tenant(scope);
            // Serialize creates per project so the limit cannot be raced.
            c.execute(
                "SELECT pg_advisory_xact_lock(hashtextextended('webhooks:' || $1 || ':' || $2, 0))",
                &[&org, &project],
            )
            .await
            .map_err(db)?;
            let count: i64 = c
                .query_one(
                    "SELECT count(*) FROM runbook.webhook_subscription WHERE org_id = $1 AND project_id = $2",
                    &[&org, &project],
                )
                .await
                .map_err(db)?
                .get(0);
            if uint(count)? >= limit as u64 {
                return Err(StoreError::InvalidBatch("webhooks.limit_reached"));
            }
            put_replay(c, scope, replay).await?;
            c.execute(
                "INSERT INTO runbook.webhook_subscription (subscription_id, org_id, project_id, created_at_ms, body) \
                 VALUES ($1, $2, $3, $4, $5)",
                &[
                    &subscription.id.as_str(),
                    &org,
                    &project,
                    &subscription.created_at_ms,
                    &encode(&subscription)?,
                ],
            )
            .await
            .map_err(|e| {
                if is_unique_violation(&e) {
                    StoreError::InvalidBatch("webhooks.duplicate")
                } else {
                    db(e)
                }
            })?;
            Ok(())
        })
        .await
    }
    async fn save_subscription(
        &self,
        scope: &TenantScope,
        subscription: Subscription,
        replay: Option<WebhookReplay>,
    ) -> Result<(), StoreError> {
        if subscription.scope != *scope {
            return Err(StoreError::NotFound);
        }
        self.transaction(async |c| {
            let stored = lock_subscription(c, scope, &subscription.id)
                .await?
                .ok_or(StoreError::NotFound)?;
            if stored.revision != subscription.revision {
                return Err(StoreError::InvalidBatch("webhooks.conflict"));
            }
            put_replay(c, scope, replay).await?;
            let mut subscription = subscription;
            subscription.revision += 1;
            write_subscription(c, &subscription).await
        })
        .await
    }
    async fn delete_subscription(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<bool, StoreError> {
        self.transaction(async |c| {
            let (org, project) = tenant(scope);
            Ok(c.execute(
                "DELETE FROM runbook.webhook_subscription \
                     WHERE subscription_id = $1 AND org_id = $2 AND project_id = $3",
                &[&id.as_str(), &org, &project],
            )
            .await
            .map_err(db)?
                == 1)
        })
        .await
    }
    async fn find_replay(
        &self,
        scope: &TenantScope,
        key: &str,
    ) -> Result<Option<WebhookReplay>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        let c = conn.client();
        let now = now_ms(c).await?;
        let (org, project) = tenant(scope);
        c.query_opt(
            "SELECT body FROM runbook.webhook_replay WHERE org_id = $1 AND project_id = $2 \
             AND replay_key = $3 AND created_at_ms > $4",
            &[
                &org,
                &project,
                &key,
                &(now - runbook_core::start::REPLAY_RETENTION_MS),
            ],
        )
        .await
        .map_err(db)?
        .map(|r| decode(r.get(0)))
        .transpose()
    }
    async fn record_event(
        &self,
        scope: &TenantScope,
        event: EventRecord,
        deliveries: Vec<DeliveryRecord>,
        replay: Option<WebhookReplay>,
    ) -> Result<bool, StoreError> {
        self.transaction(async |c| {
            let (org, project) = tenant(scope);
            if let Some(run) = &event.execution_id {
                // A fan-out racing the run's deletion records nothing.
                c.execute(
                    &format!("SELECT pg_advisory_xact_lock_shared({RUN_EVENTS_LOCK})"),
                    &[&run.as_str()],
                )
                .await
                .map_err(db)?;
                if crate::retention::tombstoned(c, run).await? {
                    return Ok(false);
                }
            }
            let inserted = c
                .execute(
                    "INSERT INTO runbook.webhook_event (org_id, project_id, event_id, execution_id, created_at_ms, body) \
                     VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
                    &[
                        &org,
                        &project,
                        &event.event_id,
                        &event.execution_id.as_ref().map(ExecutionId::as_str),
                        &event.created_at_ms,
                        &encode(&event)?,
                    ],
                )
                .await
                .map_err(db)?;
            if inserted == 0 {
                return Ok(false);
            }
            put_replay(c, scope, replay).await?;
            for delivery in &deliveries {
                if delivery.event_id != event.event_id {
                    return Err(StoreError::InvalidBatch("webhooks.invalid_delivery"));
                }
                insert_delivery(c, scope, delivery).await?;
            }
            Ok(true)
        })
        .await
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
        self.transaction(async |c| {
            let (org, project) = tenant(scope);
            let known = c
                .query_opt(
                    "SELECT 1 FROM runbook.webhook_event WHERE org_id = $1 AND project_id = $2 AND event_id = $3",
                    &[&org, &project, &delivery.event_id],
                )
                .await
                .map_err(db)?;
            if known.is_none() {
                return Err(StoreError::NotFound);
            }
            put_replay(c, scope, replay).await?;
            insert_delivery(c, scope, &delivery).await.map_err(|e| match e {
                StoreError::InvalidBatch("webhooks.invalid_delivery") => StoreError::NotFound,
                other => other,
            })
        })
        .await
    }
    async fn get_event(
        &self,
        scope: &TenantScope,
        event_id: &str,
    ) -> Result<Option<EventRecord>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        let (org, project) = tenant(scope);
        conn.client()
            .query_opt(
                "SELECT body FROM runbook.webhook_event WHERE org_id = $1 AND project_id = $2 AND event_id = $3",
                &[&org, &project, &event_id],
            )
            .await
            .map_err(db)?
            .map(|r| decode(r.get(0)))
            .transpose()
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
        self.transaction(async |c| {
            let now = now_ms(c).await?;
            let deadline = now
                .checked_add(i64::try_from(lease_ms).map_err(|_| StoreError::InvalidBatch("queue.lease_overflow"))?)
                .ok_or(StoreError::InvalidBatch("queue.lease_overflow"))?;
            let (org, project) = tenant(scope);
            let rows = c
                .query(
                    "SELECT d.delivery_id, d.generation, d.body, e.body FROM runbook.webhook_delivery d \
                     JOIN runbook.webhook_event e USING (org_id, project_id, event_id) \
                     WHERE d.org_id = $1 AND d.project_id = $2 AND d.state = 'pending' \
                     AND d.next_attempt_at_ms <= $3 \
                     AND (d.lease_deadline_at_ms IS NULL OR d.lease_deadline_at_ms <= $3) \
                     ORDER BY d.next_attempt_at_ms, d.delivery_id LIMIT $4 \
                     FOR UPDATE OF d SKIP LOCKED",
                    &[&org, &project, &now, &int(limit as u64)?],
                )
                .await
                .map_err(db)?;
            let mut out = vec![];
            for row in rows {
                let id: &str = row.get(0);
                let generation = uint(row.get(1))? + 1;
                c.execute(
                    "UPDATE runbook.webhook_delivery SET generation = $2, lease_deadline_at_ms = $3 \
                     WHERE delivery_id = $1",
                    &[&id, &int(generation)?, &deadline],
                )
                .await
                .map_err(db)?;
                let event: EventRecord = decode(row.get(3))?;
                out.push(DeliveryLease {
                    delivery: decode(row.get(2))?,
                    generation,
                    claimed_at_ms: now,
                    body: event.body,
                });
            }
            Ok(out)
        })
        .await
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
        self.transaction(async |c| {
            let now = now_ms(c).await?;
            let (org, project) = tenant(scope);
            // F34: lock the subscription before the delivery, the order a
            // subscription delete takes (its row, then the cascade), so the
            // two serialize instead of deadlocking.
            let subscription_id: WebhookId = c
                .query_opt(
                    "SELECT subscription_id FROM runbook.webhook_delivery \
                     WHERE delivery_id = $1 AND org_id = $2 AND project_id = $3",
                    &[&id.as_str(), &org, &project],
                )
                .await
                .map_err(db)?
                .ok_or(StoreError::NotFound)?
                .get::<_, &str>(0)
                .parse()
                .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?;
            let mut subscription = lock_subscription(c, scope, &subscription_id).await?;
            let row = c
                .query_opt(
                    "SELECT generation, lease_deadline_at_ms, body FROM runbook.webhook_delivery \
                     WHERE delivery_id = $1 AND org_id = $2 AND project_id = $3 FOR UPDATE",
                    &[&id.as_str(), &org, &project],
                )
                .await
                .map_err(db)?
                .ok_or(StoreError::NotFound)?;
            let mut record: DeliveryRecord = decode(row.get(2))?;
            let deadline: Option<i64> = row.get(1);
            if record.state != DeliveryState::Pending
                || uint(row.get(0))? != generation
                || deadline.is_none_or(|t| t <= now)
                || completion.attempt.number != record.attempts + 1
            {
                return Err(StoreError::InvalidBatch("webhooks.stale_lease"));
            }
            record.attempts = completion.attempt.number;
            record.state = completion.state;
            record.next_attempt_at_ms = completion.next_attempt_at_ms;
            record.failure = completion.failure.clone();
            c.execute(
                "UPDATE runbook.webhook_delivery SET state = $2, next_attempt_at_ms = $3, \
                 lease_deadline_at_ms = NULL, body = $4 WHERE delivery_id = $1",
                &[&id.as_str(), &label(&record.state)?, &record.next_attempt_at_ms, &encode(&record)?],
            )
            .await
            .map_err(db)?;
            c.execute(
                "INSERT INTO runbook.webhook_attempt (delivery_id, number, at_ms, body) VALUES ($1, $2, $3, $4)",
                &[
                    &id.as_str(),
                    &i32::try_from(completion.attempt.number).map_err(|_| StoreError::InvalidBatch("webhooks.attempt_overflow"))?,
                    &completion.attempt.at_ms,
                    &encode(&completion.attempt)?,
                ],
            )
            .await
            .map_err(db)?;
            if let Some(ok) = completion.health
                && let Some(subscription) = subscription.as_mut()
            {
                let before = subscription.clone();
                subscription.record_attempt(
                    ok,
                    completion.attempt.at_ms,
                    completion.attempt.failure_label().as_deref(),
                );
                if *subscription != before {
                    subscription.revision += 1;
                    write_subscription(c, subscription).await?;
                }
            }
            Ok(())
        })
        .await
    }
    async fn deliveries(
        &self,
        scope: &TenantScope,
        subscription: &WebhookId,
        limit: usize,
        before: Option<(i64, DeliveryId)>,
    ) -> Result<Vec<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        let c = conn.client();
        let (org, project) = tenant(scope);
        let (before_at, before_id) = match &before {
            Some((at, id)) => (Some(*at), Some(id.as_str())),
            None => (None, None),
        };
        let rows = c
            .query(
                "SELECT delivery_id, body FROM runbook.webhook_delivery \
                 WHERE org_id = $1 AND project_id = $2 AND subscription_id = $3 \
                 AND ($4::bigint IS NULL OR (created_at_ms, delivery_id) < ($4, $5::text)) \
                 ORDER BY created_at_ms DESC, delivery_id DESC LIMIT $6",
                &[
                    &org,
                    &project,
                    &subscription.as_str(),
                    &before_at,
                    &before_id,
                    &int(limit as u64)?,
                ],
            )
            .await
            .map_err(db)?;
        let mut out = vec![];
        for row in rows {
            out.push((decode(row.get(1))?, attempts(c, row.get(0)).await?));
        }
        Ok(out)
    }
    async fn delivery(
        &self,
        scope: &TenantScope,
        id: &DeliveryId,
    ) -> Result<Option<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        let c = conn.client();
        let (org, project) = tenant(scope);
        let Some(row) = c
            .query_opt(
                "SELECT body FROM runbook.webhook_delivery WHERE delivery_id = $1 AND org_id = $2 AND project_id = $3",
                &[&id.as_str(), &org, &project],
            )
            .await
            .map_err(db)?
        else {
            return Ok(None);
        };
        Ok(Some((decode(row.get(0))?, attempts(c, id.as_str()).await?)))
    }
    async fn execution_events(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<Vec<(EventRecord, Vec<DeliveryState>)>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        let c = conn.client();
        let (org, project) = tenant(scope);
        let rows = c
            .query(
                "SELECT e.body, COALESCE(array_agg(d.state) FILTER (WHERE d.state IS NOT NULL), '{}') \
                 FROM runbook.webhook_event e \
                 LEFT JOIN runbook.webhook_delivery d USING (org_id, project_id, event_id) \
                 WHERE e.org_id = $1 AND e.project_id = $2 AND e.execution_id = $3 \
                 GROUP BY e.event_id, e.body, e.created_at_ms ORDER BY e.created_at_ms, e.event_id",
                &[&org, &project, &execution.as_str()],
            )
            .await
            .map_err(db)?;
        rows.iter()
            .map(|row| {
                let states: Vec<String> = row.get(1);
                let states = states
                    .iter()
                    .map(|s| match s.as_str() {
                        "pending" => Ok(DeliveryState::Pending),
                        "delivered" => Ok(DeliveryState::Delivered),
                        "failed" => Ok(DeliveryState::Failed),
                        _ => Err(StoreError::Backend("postgres.decode_failed")),
                    })
                    .collect::<Result<_, _>>()?;
                Ok((decode(row.get(0))?, states))
            })
            .collect()
    }
    async fn purge_delivery_log(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> Result<usize, StoreError> {
        let limit = int(limit as u64)?;
        self.transaction(async |c| {
            let (org, project) = tenant(scope);
            // Finished deliveries of non-human events; human events keep
            // their delivery states as the execution's notification verdict.
            let ids: Vec<String> = c
                .query(
                    "SELECT d.delivery_id FROM runbook.webhook_delivery d \
                     JOIN runbook.webhook_event e USING (org_id, project_id, event_id) \
                     WHERE d.org_id = $1 AND d.project_id = $2 AND d.state <> 'pending' \
                     AND d.created_at_ms < $3 \
                     AND NOT COALESCE((e.body::jsonb ->> 'human')::boolean, false) \
                     ORDER BY d.created_at_ms, d.delivery_id LIMIT $4 \
                     FOR UPDATE OF d SKIP LOCKED",
                    &[&org, &project, &before_ms, &limit],
                )
                .await
                .map_err(db)?
                .iter()
                .map(|row| row.get(0))
                .collect();
            let mut removed = c
                .execute(
                    "DELETE FROM runbook.webhook_attempt WHERE delivery_id = ANY($1)",
                    &[&ids],
                )
                .await
                .map_err(db)?;
            removed += c
                .execute(
                    "DELETE FROM runbook.webhook_delivery WHERE delivery_id = ANY($1)",
                    &[&ids],
                )
                .await
                .map_err(db)?;
            // F31: recorded events past the cutoff once no delivery is kept
            // for them, and D20 replay rows past their 24 hours.
            removed += c
                .execute(
                    "DELETE FROM runbook.webhook_event WHERE (org_id, project_id, event_id) IN ( \
                     SELECT e.org_id, e.project_id, e.event_id FROM runbook.webhook_event e \
                     WHERE e.org_id = $1 AND e.project_id = $2 AND e.created_at_ms < $3 \
                     AND NOT EXISTS (SELECT 1 FROM runbook.webhook_delivery d \
                     WHERE d.org_id = e.org_id AND d.project_id = e.project_id \
                     AND d.event_id = e.event_id) \
                     ORDER BY e.created_at_ms, e.event_id LIMIT $4 FOR UPDATE SKIP LOCKED)",
                    &[&org, &project, &before_ms, &limit],
                )
                .await
                .map_err(db)?;
            let replay_cutoff = now_ms(c).await? - runbook_core::start::REPLAY_RETENTION_MS;
            removed += c
                .execute(
                    "DELETE FROM runbook.webhook_replay WHERE (org_id, project_id, replay_key) IN ( \
                     SELECT org_id, project_id, replay_key FROM runbook.webhook_replay \
                     WHERE org_id = $1 AND project_id = $2 AND created_at_ms <= $3 \
                     ORDER BY created_at_ms, replay_key LIMIT $4 FOR UPDATE SKIP LOCKED)",
                    &[&org, &project, &replay_cutoff, &limit],
                )
                .await
                .map_err(db)?;
            removed += c
                .execute(
                    "DELETE FROM runbook.webhook_attempt WHERE (delivery_id, number) IN ( \
                     SELECT a.delivery_id, a.number FROM runbook.webhook_attempt a \
                     JOIN runbook.webhook_delivery d USING (delivery_id) \
                     WHERE d.org_id = $1 AND d.project_id = $2 AND a.at_ms < $3 \
                     ORDER BY a.at_ms, a.delivery_id, a.number LIMIT $4)",
                    &[&org, &project, &before_ms, &limit],
                )
                .await
                .map_err(db)?;
            usize::try_from(removed).map_err(|_| StoreError::Backend("postgres.count_overflow"))
        })
        .await
    }
}
