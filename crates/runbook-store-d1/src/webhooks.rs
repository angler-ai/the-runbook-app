//! C1 webhooks on D1: the part that stays in D1 since CF11 moved each
//! subscription's delivery log (deliveries, attempts, retry state) into its
//! own WebhookDO (`runbook_store_do::webhook`). `runbook-worker`'s
//! `CfWebhooks` joins the two into the `WebhookStore`. Here: subscriptions,
//! the tenant's event records (one per event, whatever its subscribers),
//! replay rows and subscription health. Every query filters by
//! organization and project, and each write is one guarded batch:
//! - the subscription limit is a guarded count in the insert's batch;
//! - a save compares the stored body it read (revision and health);
//! - an event records once (a guard on its absence) and never for a
//!   tombstoned run (deletion writes the tombstone in the same batch that
//!   removes the run's events);
//! - health is written by compare-and-set on the stored body.
//!
//! The clock is the host's (`D1::now_ms`).
use crate::sql::{Columns, D1, D1Error, Row, Statement, guard, json, prepare, uint};
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    webhooks::{EventRecord, Subscription, WebhookReplay},
};
use runbook_model::{ExecutionId, WebhookId};

#[derive(Clone)]
pub struct D1Webhooks<D: D1> {
    db: D,
}
impl<D: D1> D1Webhooks<D> {
    pub fn new(db: D) -> Self {
        Self { db }
    }
}

pub(crate) fn err(error: D1Error) -> StoreError {
    crate::store_error(error)
}
fn decode<T: serde::de::DeserializeOwned>(row: &Row, index: usize) -> Result<T, StoreError> {
    row.json(index).map_err(err)
}
fn limit_value(limit: usize) -> Result<crate::Value, StoreError> {
    uint(u64::try_from(limit).unwrap_or(u64::MAX).min(1 << 40)).map_err(err)
}

/// The replay row's statements (expired rows give way to a new use of the
/// key, D20). A live row with the key fails the batch as a duplicate.
fn replay_statements(
    scope: &TenantScope,
    replay: Option<&WebhookReplay>,
) -> Result<Vec<Statement>, StoreError> {
    let Some(replay) = replay else {
        return Ok(vec![]);
    };
    let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
    Ok(vec![
        prepare(
            "DELETE FROM webhook_replay WHERE org_id = ? AND project_id = ? AND replay_key = ? \
             AND created_at_ms <= ?",
        )
        .bind(org)
        .bind(project)
        .bind(&replay.key)
        .bind(replay.created_at_ms - runbook_core::start::REPLAY_RETENTION_MS),
        guard(
            "webhooks.replay_exists",
            "NOT EXISTS (SELECT 1 FROM webhook_replay WHERE org_id = ? AND project_id = ? \
             AND replay_key = ?)",
        )
        .bind(org)
        .bind(project)
        .bind(&replay.key),
        prepare(
            "INSERT INTO webhook_replay (org_id, project_id, replay_key, created_at_ms, body) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(org)
        .bind(project)
        .bind(&replay.key)
        .bind(replay.created_at_ms)
        .bind(json(replay).map_err(err)?),
    ])
}

/// Map a write batch's failure: a guard's code is the error code, a racing
/// primary key is a duplicate.
fn write_error(error: D1Error, duplicate: &'static str) -> StoreError {
    match error {
        D1Error::Unique => StoreError::InvalidBatch(duplicate),
        D1Error::Guard(code) => StoreError::InvalidBatch(crate::static_code(&code)),
        other => err(other),
    }
}

/// Attempts at a health write before it gives up to a concurrent writer
/// (each retry re-reads and re-applies).
const COMPLETE_ATTEMPTS: usize = 5;

impl<D: D1> D1Webhooks<D> {
    async fn subscription_row(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<Option<(Subscription, String)>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM webhook_subscription WHERE subscription_id = ? \
                     AND org_id = ? AND project_id = ?",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| Ok((decode(row, 0)?, row.string(0).map_err(err)?)))
            .transpose()
    }
    pub async fn webhook_time_ms(&self) -> Result<i64, StoreError> {
        Ok(self.db.now_ms())
    }
    pub async fn list_subscriptions(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<Subscription>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM webhook_subscription WHERE org_id = ? AND project_id = ? \
                     ORDER BY created_at_ms, subscription_id",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| decode(row, 0))
            .collect()
    }
    pub async fn get_subscription(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<Option<Subscription>, StoreError> {
        Ok(self.subscription_row(scope, id).await?.map(|(s, _)| s))
    }
    pub async fn create_subscription(
        &self,
        scope: &TenantScope,
        subscription: Subscription,
        limit: usize,
        replay: Option<WebhookReplay>,
    ) -> Result<(), StoreError> {
        if subscription.scope != *scope {
            return Err(StoreError::InvalidBatch("webhooks.duplicate"));
        }
        let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
        let mut batch = vec![
            guard(
                "webhooks.limit_reached",
                "(SELECT count(*) FROM webhook_subscription WHERE org_id = ? AND project_id = ?) < ?",
            )
            .bind(org)
            .bind(project)
            .bind(limit_value(limit)?),
        ];
        batch.extend(replay_statements(scope, replay.as_ref())?);
        batch.push(
            prepare(
                "INSERT INTO webhook_subscription (subscription_id, org_id, project_id, \
                 created_at_ms, body) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(subscription.id.as_str())
            .bind(org)
            .bind(project)
            .bind(subscription.created_at_ms)
            .bind(json(&subscription).map_err(err)?),
        );
        self.db
            .batch(batch)
            .await
            .map(|_| ())
            .map_err(|e| write_error(e, "webhooks.duplicate"))
    }
    pub async fn save_subscription(
        &self,
        scope: &TenantScope,
        subscription: Subscription,
        replay: Option<WebhookReplay>,
    ) -> Result<(), StoreError> {
        if subscription.scope != *scope {
            return Err(StoreError::NotFound);
        }
        let (stored, body) = self
            .subscription_row(scope, &subscription.id)
            .await?
            .ok_or(StoreError::NotFound)?;
        if stored.revision != subscription.revision {
            return Err(StoreError::InvalidBatch("webhooks.conflict"));
        }
        let mut subscription = subscription;
        subscription.revision += 1;
        let mut batch = vec![
            guard(
                "webhooks.conflict",
                "EXISTS (SELECT 1 FROM webhook_subscription WHERE subscription_id = ? AND body = ?)",
            )
            .bind(subscription.id.as_str())
            .bind(body),
        ];
        batch.extend(replay_statements(scope, replay.as_ref())?);
        batch.push(
            prepare("UPDATE webhook_subscription SET body = ? WHERE subscription_id = ?")
                .bind(json(&subscription).map_err(err)?)
                .bind(subscription.id.as_str()),
        );
        match self.db.batch(batch).await {
            Ok(_) => Ok(()),
            Err(D1Error::Guard(code)) if code == "webhooks.conflict" => {
                // Gone, or saved by someone else since the read.
                match self.subscription_row(scope, &subscription.id).await? {
                    None => Err(StoreError::NotFound),
                    Some(_) => Err(StoreError::InvalidBatch("webhooks.conflict")),
                }
            }
            Err(error) => Err(write_error(error, "webhooks.duplicate")),
        }
    }
    pub async fn delete_subscription(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<bool, StoreError> {
        // Attempts go with their deliveries, deliveries with the
        // subscription (ON DELETE CASCADE). D1 counts cascaded rows in
        // `changes`, so the deleted row is read back with RETURNING.
        // Gate round 2: the deletion marker commits in the same batch; it
        // is what lets the subscription's object retire (positive evidence).
        let outcomes = self
            .db
            .batch(vec![
                prepare(
                    "INSERT INTO webhook_subscription_deleted (subscription_id, org_id, \
                     project_id, deleted_at_ms) SELECT subscription_id, org_id, project_id, ? \
                     FROM webhook_subscription WHERE subscription_id = ? AND org_id = ? \
                     AND project_id = ? ON CONFLICT DO NOTHING",
                )
                .bind(self.db.now_ms())
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
                prepare(
                    "DELETE FROM webhook_subscription WHERE subscription_id = ? AND org_id = ? \
                     AND project_id = ? RETURNING subscription_id",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            ])
            .await
            .map_err(err)?;
        Ok(outcomes.get(1).is_some_and(|o| !o.rows.is_empty()))
    }
    /// Gate round 2: whether D1 recorded `id`'s deletion (its marker).
    pub async fn subscription_deleted(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
    ) -> Result<bool, StoreError> {
        Ok(!self
            .db
            .all(
                prepare(
                    "SELECT 1 FROM webhook_subscription_deleted WHERE subscription_id = ? \
                     AND org_id = ? AND project_id = ?",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?
            .is_empty())
    }
    pub async fn find_replay(
        &self,
        scope: &TenantScope,
        key: &str,
    ) -> Result<Option<WebhookReplay>, StoreError> {
        let now = self.db.now_ms();
        self.db
            .all(
                prepare(
                    "SELECT body FROM webhook_replay WHERE org_id = ? AND project_id = ? \
                     AND replay_key = ? AND created_at_ms > ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(key)
                .bind(now - runbook_core::start::REPLAY_RETENTION_MS),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| decode(row, 0))
            .transpose()
    }
    pub async fn get_event(
        &self,
        scope: &TenantScope,
        event_id: &str,
    ) -> Result<Option<EventRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM webhook_event WHERE org_id = ? AND project_id = ? \
                     AND event_id = ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(event_id),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| decode(row, 0))
            .transpose()
    }

    /// Record `event` for the tenant once (false when it is already
    /// recorded, or its run was deleted), with the replay row.
    pub async fn record_event_row(
        &self,
        scope: &TenantScope,
        event: &EventRecord,
        replay: Option<&WebhookReplay>,
    ) -> Result<bool, StoreError> {
        let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
        let mut batch = vec![
            guard(
                "recorded",
                "NOT EXISTS (SELECT 1 FROM webhook_event WHERE org_id = ? AND project_id = ? \
                 AND event_id = ?)",
            )
            .bind(org)
            .bind(project)
            .bind(&event.event_id),
        ];
        if let Some(run) = &event.execution_id {
            // A fan-out racing the run's deletion records nothing.
            batch.push(
                guard(
                    "recorded",
                    "NOT EXISTS (SELECT 1 FROM tombstone WHERE execution_id = ?)",
                )
                .bind(run.as_str()),
            );
        }
        batch.push(
            prepare(
                "INSERT INTO webhook_event (org_id, project_id, event_id, execution_id, \
                 created_at_ms, body) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(org)
            .bind(project)
            .bind(&event.event_id)
            .bind(event.execution_id.as_ref().map(|e| e.as_str().to_owned()))
            .bind(event.created_at_ms)
            .bind(json(event).map_err(err)?),
        );
        batch.extend(replay_statements(scope, replay)?);
        match self.db.batch(batch).await {
            Ok(_) => Ok(true),
            Err(D1Error::Guard(code)) if code == "recorded" => Ok(false),
            Err(error) => Err(write_error(error, "webhooks.duplicate")),
        }
    }
    /// Whether the tenant recorded `event_id` and its run was not deleted.
    pub async fn event_recorded(
        &self,
        scope: &TenantScope,
        event_id: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.get_event(scope, event_id).await?.is_some())
    }
    /// Write one replay row (a redelivery's, after its delivery is in the
    /// subscription's log).
    /// Gate fix (low): take back a redelivery's replay row when the
    /// delivery it recorded could not be added.
    pub async fn delete_replay(&self, scope: &TenantScope, key: &str) -> Result<(), StoreError> {
        self.db
            .run(
                prepare(
                    "DELETE FROM webhook_replay WHERE org_id = ? AND project_id = ? \
                     AND replay_key = ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(key),
            )
            .await
            .map(|_| ())
            .map_err(err)
    }
    pub async fn write_replay(
        &self,
        scope: &TenantScope,
        replay: &WebhookReplay,
    ) -> Result<(), StoreError> {
        self.db
            .batch(replay_statements(scope, Some(replay))?)
            .await
            .map(|_| ())
            .map_err(|e| write_error(e, "webhooks.duplicate"))
    }
    /// Every event recorded for one execution, oldest first.
    pub async fn execution_event_records(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<Vec<EventRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM webhook_event WHERE org_id = ? AND project_id = ? \
                     AND execution_id = ? ORDER BY created_at_ms, event_id",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(execution.as_str()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| decode(row, 0))
            .collect()
    }
    /// One attempt's effect on its subscription's health (compare-and-set
    /// on the stored body, retried against a concurrent writer).
    pub async fn record_health(
        &self,
        scope: &TenantScope,
        id: &WebhookId,
        ok: bool,
        at_ms: i64,
        failure: Option<&str>,
    ) -> Result<(), StoreError> {
        for _ in 0..COMPLETE_ATTEMPTS {
            let Some((stored, body)) = self.subscription_row(scope, id).await? else {
                return Ok(());
            };
            let mut subscription = stored.clone();
            subscription.record_attempt(ok, at_ms, failure);
            if subscription == stored {
                return Ok(());
            }
            subscription.revision += 1;
            let batch = vec![
                guard(
                    "health",
                    "EXISTS (SELECT 1 FROM webhook_subscription WHERE subscription_id = ? \
                     AND body = ?)",
                )
                .bind(subscription.id.as_str())
                .bind(body),
                prepare("UPDATE webhook_subscription SET body = ? WHERE subscription_id = ?")
                    .bind(json(&subscription).map_err(err)?)
                    .bind(subscription.id.as_str()),
            ];
            match self.db.batch(batch).await {
                Ok(_) => return Ok(()),
                Err(D1Error::Guard(code)) if code == "health" => continue,
                Err(error) => return Err(err(error)),
            }
        }
        Err(StoreError::Backend("d1.retry"))
    }
    /// D1's share of delivery-log retention: event records of events that
    /// are not human, created before `before_ms` (their deliveries keep a
    /// copy in their subscriptions' logs), and replay rows past their 24
    /// hours. Bounded by `limit` rows of each kind.
    pub async fn purge_events(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> Result<usize, StoreError> {
        let limit = limit_value(limit)?;
        let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
        let replay_cutoff = self.db.now_ms() - runbook_core::start::REPLAY_RETENTION_MS;
        let outcomes = self
            .db
            .batch(vec![
                prepare(
                    "DELETE FROM webhook_event WHERE rowid IN (SELECT rowid FROM webhook_event \
                     WHERE org_id = ? AND project_id = ? AND created_at_ms < ? \
                     AND NOT coalesce(json_extract(body, '$.human'), 0) \
                     ORDER BY created_at_ms, event_id LIMIT ?)",
                )
                .bind(org)
                .bind(project)
                .bind(before_ms)
                .bind(limit.clone()),
                prepare(
                    "DELETE FROM webhook_replay WHERE rowid IN (SELECT rowid FROM webhook_replay \
                     WHERE org_id = ? AND project_id = ? AND created_at_ms <= ? \
                     ORDER BY created_at_ms, replay_key LIMIT ?)",
                )
                .bind(org)
                .bind(project)
                .bind(replay_cutoff)
                .bind(limit),
            ])
            .await
            .map_err(err)?;
        let removed: u64 = outcomes.iter().map(|o| o.changes).sum();
        usize::try_from(removed).map_err(|_| StoreError::Backend("d1.count_overflow"))
    }
}
