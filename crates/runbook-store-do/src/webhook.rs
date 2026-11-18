//! CF11: the WebhookDO's store, one webhook subscription's delivery log
//! (deliveries, attempts, retry and backoff state), moved out of D1 by the
//! user's decision (superseding D56's placement). The subscription itself,
//! the tenant's event records and the replay rows stay in D1; the composed
//! store in `runbook-worker` (`webhooks.rs`) joins the two.
//!
//! Each call is one transaction in a single-threaded object, with the
//! rules of the D1 and Postgres adapters: first deliveries are unique per
//! event, claims are leases fenced by generation, a completion must hold
//! the lease, and retention drops finished deliveries of non-human events
//! (a human event's states are its run's notified verdict) and old
//! attempts, never a pending delivery.
use crate::{
    DoSql,
    migrate::{Migration, migrate_with},
    sql::{Columns, Value, decode, encode, int, uint},
};
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    webhooks::{AttemptCompletion, AttemptRecord, DeliveryLease, DeliveryRecord, EventRecord},
};
use runbook_model::{DeliveryId, DeliveryState, WebhookId};
use std::collections::BTreeMap;

/// Every WebhookDO migration, in order.
pub const WEBHOOK_MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_delivery_log.sql",
        sql: include_str!("../../../cloudflare/migrations/webhookdo/0001_delivery_log.sql"),
    },
    Migration {
        version: 2,
        name: "0002_lifecycle.sql",
        sql: include_str!("../../../cloudflare/migrations/webhookdo/0002_lifecycle.sql"),
    },
];

/// Gate fix D3: the alarm never fires sooner than this after it is set.
pub const ALARM_FLOOR_MS: i64 = 1_000;
/// Gate fix D3: the longest wait after consecutive failed passes.
pub const ALARM_MAX_BACKOFF_MS: i64 = 300_000;

/// What the alarm time follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmResult {
    /// A write or read: keep any backoff, but never push a due alarm later
    /// (a client polling the log must not starve its own deliveries).
    Rearm,
    /// An alarm pass that worked: clear the backoff.
    Succeeded,
    /// An alarm pass that failed: back off further.
    Failed,
}

/// Gate fix D3: the object's next alarm. `next_due` is its log's next due
/// delivery. After an alarm pass it is never sooner than
/// [`ALARM_FLOOR_MS`] from `now` (an overdue delivery does not loop the
/// alarm), and after `n` failed passes in a row not sooner than `2^n`
/// seconds (up to [`ALARM_MAX_BACKOFF_MS`]). A write or read keeps that
/// backoff but adds no floor, so an overdue delivery's alarm fires at once
/// (its pass then floors) and repeated reads never push it later. Nothing
/// due, no alarm.
pub fn alarm_at<B: DoSql>(
    sql: &B,
    next_due: Option<i64>,
    now_ms: i64,
    result: AlarmResult,
) -> Result<Option<i64>, StoreError> {
    migrate_with(sql, WEBHOOK_MIGRATIONS)?;
    let floor = now_ms.saturating_add(ALARM_FLOOR_MS);
    let until = match result {
        AlarmResult::Succeeded => {
            sql.exec("DELETE FROM alarm_backoff", &[])?;
            floor
        }
        AlarmResult::Failed => {
            let failures = sql
                .query("SELECT failures FROM alarm_backoff WHERE id = 1", &[])?
                .first()
                .map(|row| row.integer(0))
                .transpose()?
                .unwrap_or(0)
                .saturating_add(1);
            let wait = ALARM_FLOOR_MS
                .saturating_mul(
                    1_i64
                        .checked_shl(u32::try_from(failures.min(20)).unwrap_or(20))
                        .unwrap_or(i64::MAX),
                )
                .min(ALARM_MAX_BACKOFF_MS);
            let until = now_ms.saturating_add(wait);
            sql.exec(
                "INSERT INTO alarm_backoff (id, failures, until_ms) VALUES (1, ?, ?) \
                 ON CONFLICT (id) DO UPDATE SET failures = excluded.failures, \
                 until_ms = excluded.until_ms",
                &[failures.into(), until.into()],
            )?;
            until
        }
        AlarmResult::Rearm => sql
            .query("SELECT until_ms FROM alarm_backoff WHERE id = 1", &[])?
            .first()
            .map(|row| row.integer(0))
            .transpose()?
            .unwrap_or(i64::MIN),
    };
    Ok(next_due.map(|due| due.max(until)))
}

fn invalid(code: &'static str) -> StoreError {
    StoreError::InvalidBatch(code)
}
fn state_label(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Pending => "pending",
        DeliveryState::Delivered => "delivered",
        DeliveryState::Failed => "failed",
    }
}
fn state_of(text: &str) -> Result<DeliveryState, StoreError> {
    match text {
        "pending" => Ok(DeliveryState::Pending),
        "delivered" => Ok(DeliveryState::Delivered),
        "failed" => Ok(DeliveryState::Failed),
        _ => Err(StoreError::Backend("do.decode")),
    }
}
fn opt(value: Option<i64>) -> Result<Value, StoreError> {
    value.map(int).transpose().map(|v| v.unwrap_or(Value::Null))
}

/// One subscription's delivery log.
#[derive(Clone)]
pub struct WebhookLog<B: DoSql> {
    sql: B,
    scope: TenantScope,
    subscription: WebhookId,
}

impl<B: DoSql> WebhookLog<B> {
    /// Open the log of `subscription` in `scope`, bringing its schema up to
    /// date. An object that already holds another subscription (or another
    /// tenant's) is `NotFound`.
    pub fn open(sql: B, scope: TenantScope, subscription: WebhookId) -> Result<Self, StoreError> {
        migrate_with(&sql, WEBHOOK_MIGRATIONS)?;
        // Gate fix D3: a retired object (its subscription deleted) refuses
        // everything, so a late write cannot bring it back.
        if !sql
            .query("SELECT 1 FROM retired WHERE id = 1", &[])?
            .is_empty()
        {
            return Err(StoreError::NotFound);
        }
        let rows = sql.query(
            "SELECT org_id, project_id, subscription_id FROM owner WHERE id = 1",
            &[],
        )?;
        if let Some(row) = rows.first()
            && (row.text(0)? != scope.org_id.as_str()
                || row.text(1)? != scope.project_id.as_str()
                || row.text(2)? != subscription.as_str())
        {
            return Err(StoreError::NotFound);
        }
        Ok(Self {
            sql,
            scope,
            subscription,
        })
    }

    fn claim_owner(&self) -> Result<(), StoreError> {
        self.sql.exec(
            "INSERT INTO owner (id, org_id, project_id, subscription_id) VALUES (1, ?, ?, ?) \
             ON CONFLICT (id) DO NOTHING",
            &[
                self.scope.org_id.as_str().into(),
                self.scope.project_id.as_str().into(),
                self.subscription.as_str().into(),
            ],
        )
    }
    fn check(&self, delivery: &DeliveryRecord) -> Result<(), StoreError> {
        if delivery.subscription_id != self.subscription {
            return Err(invalid("webhooks.invalid_delivery"));
        }
        Ok(())
    }
    fn insert_delivery(&self, delivery: &DeliveryRecord) -> Result<(), StoreError> {
        self.sql.exec(
            "INSERT INTO webhook_delivery (delivery_id, org_id, project_id, subscription_id, \
             event_id, redelivery_of, state, next_attempt_at_ms, created_at_ms, body) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                delivery.id.as_str().into(),
                self.scope.org_id.as_str().into(),
                self.scope.project_id.as_str().into(),
                self.subscription.as_str().into(),
                delivery.event_id.as_str().into(),
                delivery
                    .redelivery_of
                    .as_ref()
                    .map_or(Value::Null, |d| d.as_str().into()),
                state_label(delivery.state).into(),
                opt(delivery.next_attempt_at_ms)?,
                int(delivery.created_at_ms)?,
                encode(delivery)?,
            ],
        )
    }

    /// Record `event`'s first deliveries to this subscription. Idempotent:
    /// a delivery already recorded is left as it is. True when any row was
    /// written.
    pub fn record(
        &self,
        event: &EventRecord,
        deliveries: &[DeliveryRecord],
    ) -> Result<bool, StoreError> {
        for delivery in deliveries {
            self.check(delivery)?;
            if delivery.event_id != event.event_id || delivery.redelivery_of.is_some() {
                return Err(invalid("webhooks.invalid_delivery"));
            }
        }
        let (this, event, deliveries) = (self.clone(), event.clone(), deliveries.to_vec());
        self.sql.transaction(move || {
            this.claim_owner()?;
            let mut wrote = false;
            if this
                .sql
                .query(
                    "SELECT 1 FROM delivery_event WHERE event_id = ?",
                    &[event.event_id.as_str().into()],
                )?
                .is_empty()
            {
                this.sql.exec(
                    "INSERT INTO delivery_event (event_id, created_at_ms, human, body) \
                     VALUES (?, ?, ?, ?)",
                    &[
                        event.event_id.as_str().into(),
                        int(event.created_at_ms)?,
                        int(i64::from(event.human))?,
                        encode(&event)?,
                    ],
                )?;
                wrote = true;
            }
            for delivery in &deliveries {
                let exists = !this
                    .sql
                    .query(
                        "SELECT 1 FROM webhook_delivery WHERE delivery_id = ? \
                         OR (event_id = ? AND redelivery_of IS NULL)",
                        &[
                            delivery.id.as_str().into(),
                            delivery.event_id.as_str().into(),
                        ],
                    )?
                    .is_empty();
                if !exists {
                    this.insert_delivery(delivery)?;
                    wrote = true;
                }
            }
            Ok(wrote)
        })
    }

    /// A redelivery of an event this log holds (`NotFound` otherwise; a
    /// delivery ID in use is `webhooks.duplicate`).
    pub fn add(&self, delivery: &DeliveryRecord) -> Result<(), StoreError> {
        self.check(delivery)?;
        if delivery.redelivery_of.is_none() {
            return Err(invalid("webhooks.duplicate"));
        }
        let (this, delivery) = (self.clone(), delivery.clone());
        self.sql.transaction(move || {
            if this
                .sql
                .query(
                    "SELECT 1 FROM delivery_event WHERE event_id = ?",
                    &[delivery.event_id.as_str().into()],
                )?
                .is_empty()
            {
                return Err(StoreError::NotFound);
            }
            // Gate fix (low): idempotent by ID: the same redelivery again
            // (a retry) is a no-op; another one under its ID is refused.
            let existing = this.sql.query(
                "SELECT event_id, subscription_id, coalesce(redelivery_of, '') \
                 FROM webhook_delivery WHERE delivery_id = ?",
                &[delivery.id.as_str().into()],
            )?;
            if let Some(row) = existing.first() {
                let same = row.text(0)? == delivery.event_id
                    && row.text(1)? == delivery.subscription_id.as_str()
                    && row.text(2)? == delivery.redelivery_of.as_ref().map_or("", |d| d.as_str());
                return if same {
                    Ok(())
                } else {
                    Err(invalid("webhooks.duplicate"))
                };
            }
            this.insert_delivery(&delivery)
        })
    }

    /// Lease up to `limit` pending deliveries due at `now`, each with its
    /// event's body.
    pub fn claim(
        &self,
        now: i64,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<DeliveryLease>, StoreError> {
        if lease_ms == 0 {
            return Err(invalid("queue.invalid_lease"));
        }
        let deadline = i64::try_from(lease_ms)
            .ok()
            .and_then(|ms| now.checked_add(ms))
            .ok_or(invalid("queue.lease_overflow"))?;
        let this = self.clone();
        self.sql.transaction(move || {
            let due = this.sql.query(
                "SELECT d.delivery_id, d.generation, d.body, e.body FROM webhook_delivery d \
                 JOIN delivery_event e ON e.event_id = d.event_id \
                 WHERE d.state = 'pending' AND d.next_attempt_at_ms <= ? \
                 AND (d.lease_deadline_at_ms IS NULL OR d.lease_deadline_at_ms <= ?) \
                 ORDER BY d.next_attempt_at_ms, d.delivery_id LIMIT ?",
                &[int(now)?, int(now)?, uint(limit as u64)?],
            )?;
            let mut out = vec![];
            for row in &due {
                let generation = row.unsigned(1)? + 1;
                this.sql.exec(
                    "UPDATE webhook_delivery SET generation = ?, lease_deadline_at_ms = ? \
                     WHERE delivery_id = ?",
                    &[uint(generation)?, int(deadline)?, row.text(0)?.into()],
                )?;
                let event: EventRecord = decode(row.text(3)?)?;
                out.push(DeliveryLease {
                    delivery: decode(row.text(2)?)?,
                    generation,
                    claimed_at_ms: now,
                    body: event.body,
                });
            }
            Ok(out)
        })
    }

    /// Record one attempt of a delivery this caller leased at `generation`
    /// (`webhooks.stale_lease` otherwise). Returns the delivery as stored.
    pub fn complete(
        &self,
        id: &DeliveryId,
        generation: u64,
        completion: &AttemptCompletion,
        now: i64,
    ) -> Result<DeliveryRecord, StoreError> {
        let (this, id, completion) = (self.clone(), id.clone(), completion.clone());
        self.sql.transaction(move || {
            let rows = this.sql.query(
                "SELECT generation, lease_deadline_at_ms, body FROM webhook_delivery \
                 WHERE delivery_id = ?",
                &[id.as_str().into()],
            )?;
            let row = rows.first().ok_or(StoreError::NotFound)?;
            let mut record: DeliveryRecord = decode(row.text(2)?)?;
            let deadline = row.optional_integer(1)?;
            if record.state != DeliveryState::Pending
                || row.unsigned(0)? != generation
                || deadline.is_none_or(|t| t <= now)
                || completion.attempt.number != record.attempts + 1
            {
                return Err(invalid("webhooks.stale_lease"));
            }
            record.attempts = completion.attempt.number;
            record.state = completion.state;
            record.next_attempt_at_ms = completion.next_attempt_at_ms;
            record.failure = completion.failure.clone();
            this.sql.exec(
                "UPDATE webhook_delivery SET state = ?, next_attempt_at_ms = ?, \
                 lease_deadline_at_ms = NULL, body = ? WHERE delivery_id = ?",
                &[
                    state_label(record.state).into(),
                    opt(record.next_attempt_at_ms)?,
                    encode(&record)?,
                    id.as_str().into(),
                ],
            )?;
            this.sql.exec(
                "INSERT INTO webhook_attempt (delivery_id, number, at_ms, body) \
                 VALUES (?, ?, ?, ?)",
                &[
                    id.as_str().into(),
                    int(i64::from(completion.attempt.number))?,
                    int(completion.attempt.at_ms)?,
                    encode(&completion.attempt)?,
                ],
            )?;
            Ok(record)
        })
    }

    fn with_attempts(
        &self,
        deliveries: Vec<DeliveryRecord>,
    ) -> Result<Vec<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError> {
        let mut out = vec![];
        for delivery in deliveries {
            let attempts = self
                .sql
                .query(
                    "SELECT body FROM webhook_attempt WHERE delivery_id = ? ORDER BY number",
                    &[delivery.id.as_str().into()],
                )?
                .iter()
                .map(|row| decode(row.text(0)?))
                .collect::<Result<_, _>>()?;
            out.push((delivery, attempts));
        }
        Ok(out)
    }

    /// Newest first, strictly older than `before` (created_at_ms, id).
    pub fn deliveries(
        &self,
        limit: usize,
        before: Option<(i64, DeliveryId)>,
    ) -> Result<Vec<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError> {
        let rows = match &before {
            None => self.sql.query(
                "SELECT body FROM webhook_delivery \
                 ORDER BY created_at_ms DESC, delivery_id DESC LIMIT ?",
                &[uint(limit as u64)?],
            )?,
            Some((at, id)) => self.sql.query(
                "SELECT body FROM webhook_delivery WHERE created_at_ms < ? \
                 OR (created_at_ms = ? AND delivery_id < ?) \
                 ORDER BY created_at_ms DESC, delivery_id DESC LIMIT ?",
                &[
                    int(*at)?,
                    int(*at)?,
                    id.as_str().into(),
                    uint(limit as u64)?,
                ],
            )?,
        };
        let deliveries = rows
            .iter()
            .map(|row| decode(row.text(0)?))
            .collect::<Result<_, _>>()?;
        self.with_attempts(deliveries)
    }

    pub fn delivery(
        &self,
        id: &DeliveryId,
    ) -> Result<Option<(DeliveryRecord, Vec<AttemptRecord>)>, StoreError> {
        let rows = self.sql.query(
            "SELECT body FROM webhook_delivery WHERE delivery_id = ?",
            &[id.as_str().into()],
        )?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        Ok(self.with_attempts(vec![decode(row.text(0)?)?])?.pop())
    }

    /// The delivery states of each of `events` this log holds.
    pub fn states(
        &self,
        events: &[String],
    ) -> Result<BTreeMap<String, Vec<DeliveryState>>, StoreError> {
        let mut out: BTreeMap<String, Vec<DeliveryState>> = BTreeMap::new();
        for event in events {
            for row in self.sql.query(
                "SELECT state FROM webhook_delivery WHERE event_id = ? ORDER BY delivery_id",
                &[event.as_str().into()],
            )? {
                out.entry(event.clone())
                    .or_default()
                    .push(state_of(row.text(0)?)?);
            }
        }
        Ok(out)
    }

    /// Retention (native rules, `DELIVERY_LOG_RETENTION_MS`): finished
    /// deliveries created before `before_ms` of events that are not human,
    /// with their attempts; then events left with no delivery; then attempts
    /// logged before `before_ms`. Each kind is bounded by `limit`; pending
    /// deliveries are never touched. Returns how many rows went.
    pub fn purge(&self, before_ms: i64, limit: usize) -> Result<usize, StoreError> {
        let this = self.clone();
        self.sql.transaction(move || {
            let limit = uint(limit as u64)?;
            let ids: Vec<String> = this
                .sql
                .query(
                    "SELECT d.delivery_id FROM webhook_delivery d \
                     JOIN delivery_event e ON e.event_id = d.event_id \
                     WHERE d.state <> 'pending' AND d.created_at_ms < ? AND e.human = 0 \
                     ORDER BY d.created_at_ms, d.delivery_id LIMIT ?",
                    &[int(before_ms)?, limit.clone()],
                )?
                .iter()
                .map(|row| row.text(0).map(str::to_owned))
                .collect::<Result<_, _>>()?;
            let mut removed = 0u64;
            for id in &ids {
                removed += this.count(
                    "SELECT count(*) FROM webhook_attempt WHERE delivery_id = ?",
                    id,
                )?;
                this.sql.exec(
                    "DELETE FROM webhook_attempt WHERE delivery_id = ?",
                    &[id.as_str().into()],
                )?;
                this.sql.exec(
                    "DELETE FROM webhook_delivery WHERE delivery_id = ?",
                    &[id.as_str().into()],
                )?;
                removed += 1;
            }
            let orphans = this.sql.query(
                "SELECT event_id FROM delivery_event e WHERE NOT EXISTS \
                 (SELECT 1 FROM webhook_delivery d WHERE d.event_id = e.event_id) \
                 ORDER BY created_at_ms, event_id LIMIT ?",
                std::slice::from_ref(&limit),
            )?;
            for row in &orphans {
                this.sql.exec(
                    "DELETE FROM delivery_event WHERE event_id = ?",
                    &[row.text(0)?.into()],
                )?;
            }
            let old = this.sql.query(
                "SELECT delivery_id, number FROM webhook_attempt WHERE at_ms < ? \
                 ORDER BY at_ms, delivery_id, number LIMIT ?",
                &[int(before_ms)?, limit],
            )?;
            for row in &old {
                this.sql.exec(
                    "DELETE FROM webhook_attempt WHERE delivery_id = ? AND number = ?",
                    &[row.text(0)?.into(), int(row.integer(1)?)?],
                )?;
                removed += 1;
            }
            usize::try_from(removed).map_err(|_| StoreError::Backend("do.count_overflow"))
        })
    }

    fn count(&self, sql: &str, id: &str) -> Result<u64, StoreError> {
        self.sql
            .query(sql, &[id.into()])?
            .first()
            .map(|row| row.unsigned(0))
            .transpose()
            .map(|n| n.unwrap_or(0))
    }

    /// When this log next has work: the earliest a pending delivery becomes
    /// claimable (its next attempt, or its lease's end), for the alarm.
    pub fn next_due(&self) -> Result<Option<i64>, StoreError> {
        self.sql
            .query(
                "SELECT min(max(coalesce(next_attempt_at_ms, 0), \
                 coalesce(lease_deadline_at_ms, 0))) FROM webhook_delivery \
                 WHERE state = 'pending'",
                &[],
            )?
            .first()
            .map(|row| row.optional_integer(0))
            .transpose()
            .map(Option::flatten)
    }

    /// Forget every delivery of `execution` (its run was deleted): the
    /// deliveries, their attempts and the event copies left with none.
    pub fn forget_execution(&self, execution: &str) -> Result<usize, StoreError> {
        let (sql, execution) = (self.sql.clone(), execution.to_owned());
        self.sql.transaction(move || {
            let ids: Vec<String> = sql
                .query(
                    "SELECT delivery_id FROM webhook_delivery \
                     WHERE json_extract(body, '$.execution_id') = ?",
                    &[execution.as_str().into()],
                )?
                .iter()
                .map(|row| row.text(0).map(str::to_owned))
                .collect::<Result<_, _>>()?;
            for id in &ids {
                sql.exec(
                    "DELETE FROM webhook_attempt WHERE delivery_id = ?",
                    &[id.as_str().into()],
                )?;
                sql.exec(
                    "DELETE FROM webhook_delivery WHERE delivery_id = ?",
                    &[id.as_str().into()],
                )?;
            }
            sql.exec(
                "DELETE FROM delivery_event WHERE json_extract(body, '$.execution_id') = ?",
                &[execution.as_str().into()],
            )?;
            Ok(ids.len())
        })
    }

    /// Empty the object (its subscription was deleted).
    pub fn purge_all(&self) -> Result<(), StoreError> {
        let sql = self.sql.clone();
        self.sql.transaction(move || {
            for table in [
                "webhook_attempt",
                "webhook_delivery",
                "delivery_event",
                "owner",
                "alarm_backoff",
            ] {
                sql.exec(&format!("DELETE FROM {table}"), &[])?;
            }
            // Gate fix D3: retired for good (subscription IDs are never
            // reused).
            sql.exec(
                "INSERT INTO retired (id, at_ms) VALUES (1, ?) ON CONFLICT (id) DO NOTHING",
                &[sql.now_ms().into()],
            )?;
            Ok(())
        })
    }
}

/// The subscription an object holds (its first write fixed it), for the
/// object's alarm, which arrives with no request.
pub fn stored_owner<B: DoSql>(sql: &B) -> Result<Option<(TenantScope, WebhookId)>, StoreError> {
    migrate_with(sql, WEBHOOK_MIGRATIONS)?;
    let rows = sql.query(
        "SELECT org_id, project_id, subscription_id FROM owner WHERE id = 1",
        &[],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let bad = |_| StoreError::Backend("do.invalid_stored_id");
    Ok(Some((
        TenantScope {
            org_id: row.text(0)?.parse().map_err(bad)?,
            project_id: row.text(1)?.parse().map_err(bad)?,
        },
        row.text(2)?.parse().map_err(bad)?,
    )))
}
