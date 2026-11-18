//! The outbox drain (CF2). Two streams leave an ExecutionDO, both written
//! in the same transaction as the change that caused them:
//! - [`OutboxStream::Derived`]: one `DerivedUpdate` per committed sequence
//!   (sequence 0 is the start's), which D1 applies idempotently by
//!   (execution, sequence) and ignores when older (CF3);
//! - [`OutboxStream::Notification`]: notification intents, several per
//!   sequence, keyed by event ID, fanned out to webhooks and people (CF4).
//!
//! **Contract.** A consumer lists pending entries in cursor order
//! (`pending_*`, optionally after a cursor it already holds), delivers
//! them, then acknowledges through the cursor of the last entry it
//! delivered. A derived entry's cursor is its sequence (one per sequence);
//! a notification's is `(sequence, event_id)` ([`NotificationCursor`]),
//! because one sequence can carry several intents and a page may end
//! inside it: acking by sequence alone would ack intents the page never
//! held. Acks are idempotent, cumulative and never go backwards; an ack
//! past the head is refused. Entries stay stored after their ack
//! (they are part of the run until retention deletes it); only the pending
//! view shrinks. A notification acknowledged through the work queue leaves
//! the pending view too, so the two paths never double-deliver.
//!
//! **Retries.** The host delivers right after each commit. Undelivered
//! entries wake the object [`OUTBOX_FIRST_RETRY_MS`] after the oldest one
//! was written; a host whose delivery fails calls [`DoStore::defer_outbox`]
//! to back the stream off (doubling up to [`OUTBOX_MAX_RETRY_MS`]). Any ack
//! clears the backoff.
use crate::{
    DoSql, DoStore,
    sql::{Columns, Value, decode, int, uint},
};
use runbook_core::{StoreError, catalog::DerivedUpdate};
use runbook_engine::NotificationIntent;

/// How long after the oldest undelivered entry the alarm retries it.
pub const OUTBOX_FIRST_RETRY_MS: i64 = 10_000;
/// The soonest an outbox retry is armed from now (see `next_alarm_at`).
pub const OUTBOX_MIN_RETRY_MS: i64 = 1_000;
/// The longest backoff between failed delivery attempts.
pub const OUTBOX_MAX_RETRY_MS: i64 = 600_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OutboxStream {
    Notification,
    Derived,
}
impl OutboxStream {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Notification => "notification",
            Self::Derived => "derived",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "notification" => Some(Self::Notification),
            "derived" => Some(Self::Derived),
            _ => None,
        }
    }
    fn table(self) -> &'static str {
        match self {
            Self::Notification => "outbox",
            Self::Derived => "derived_outbox",
        }
    }
}

/// Where a notification consumer is in the stream: the last intent it
/// holds. Order is `(sequence, event_id)`, the order `pending_notifications`
/// returns.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NotificationCursor {
    pub sequence: u64,
    pub event_id: String,
}
impl NotificationCursor {
    pub fn of(intent: &NotificationIntent) -> Self {
        Self {
            sequence: intent.sequence,
            event_id: intent.event_id.clone(),
        }
    }
}

fn page_limit(limit: usize) -> Result<Value, StoreError> {
    uint(u64::try_from(limit).unwrap_or(u64::MAX).min(10_000))
}

/// When the stream's undelivered entries next need the object awake.
pub(crate) fn retry_at<B: DoSql>(
    store: &DoStore<B>,
    stream: OutboxStream,
) -> Result<Option<i64>, StoreError> {
    let oldest = store.sql.query(
        &format!(
            "SELECT MIN(at_ms) FROM {} WHERE acked_at_ms IS NULL",
            stream.table()
        ),
        &[],
    )?;
    let Some(oldest) = oldest
        .first()
        .map(|row| row.optional_integer(0))
        .transpose()?
    else {
        return Ok(None);
    };
    let Some(oldest) = oldest else {
        return Ok(None);
    };
    let backoff = store.sql.query(
        "SELECT next_attempt_at_ms FROM outbox_retry WHERE stream = ?",
        &[stream.as_str().into()],
    )?;
    let first = oldest.saturating_add(OUTBOX_FIRST_RETRY_MS);
    Ok(Some(match backoff.first() {
        Some(row) => first.max(row.integer(0)?),
        None => first,
    }))
}

impl<B: DoSql> DoStore<B> {
    /// Undelivered derived updates in sequence order, after `after` if given.
    pub fn pending_derived(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<(u64, DerivedUpdate)>, StoreError> {
        if self.own_head()?.is_none() {
            return Err(StoreError::NotFound);
        }
        // Gate fix D2: nothing reaches D1 before the import commits.
        if self.held()? {
            return Ok(vec![]);
        }
        let (clause, mut params) = match after {
            Some(sequence) => (" AND sequence > ?", vec![uint(sequence)?]),
            None => ("", vec![]),
        };
        params.push(page_limit(limit)?);
        self.sql
            .query(
                &format!(
                    "SELECT sequence, entry FROM derived_outbox WHERE acked_at_ms IS NULL{clause} \
                     ORDER BY sequence LIMIT ?"
                ),
                &params,
            )?
            .iter()
            .map(|row| {
                let sequence = row.unsigned(0)?;
                Ok((sequence, self.read_derived(sequence, row.text(1)?)?))
            })
            .collect()
    }

    /// One derived row by sequence, delivered or not (CF3: the D1 applier
    /// redelivers a recorded row; it is idempotent by sequence).
    pub fn derived_row(&self, sequence: u64) -> Result<Option<DerivedUpdate>, StoreError> {
        if self.own_head()?.is_none() {
            return Err(StoreError::NotFound);
        }
        self.sql
            .query(
                "SELECT entry FROM derived_outbox WHERE sequence = ?",
                &[uint(sequence)?],
            )?
            .first()
            .map(|row| self.read_derived(sequence, row.text(0)?))
            .transpose()
    }

    /// Undelivered notification intents in (sequence, event ID) order,
    /// after `after` if given.
    pub fn pending_notifications(
        &self,
        after: Option<&NotificationCursor>,
        limit: usize,
    ) -> Result<Vec<NotificationIntent>, StoreError> {
        if self.own_head()?.is_none() {
            return Err(StoreError::NotFound);
        }
        // Gate fix D2: nothing is published before the import commits.
        if self.held()? {
            return Ok(vec![]);
        }
        let (clause, mut params) = match after {
            Some(cursor) => (
                " AND (sequence > ? OR (sequence = ? AND event_id > ?))",
                vec![
                    uint(cursor.sequence)?,
                    uint(cursor.sequence)?,
                    cursor.event_id.as_str().into(),
                ],
            ),
            None => ("", vec![]),
        };
        params.push(page_limit(limit)?);
        self.sql
            .query(
                &format!(
                    "SELECT intent FROM outbox WHERE acked_at_ms IS NULL{clause} \
                     ORDER BY sequence, event_id LIMIT ?"
                ),
                &params,
            )?
            .iter()
            .map(|row| decode(row.text(0)?))
            .collect()
    }

    /// Acknowledge every derived entry with sequence at most `through`.
    /// Returns how many this call newly acknowledged.
    pub fn ack_derived(&self, through: u64) -> Result<u64, StoreError> {
        self.ack(
            OutboxStream::Derived,
            through,
            "sequence <= ?".to_owned(),
            vec![uint(through)?],
        )
    }

    /// Acknowledge every notification at or before `through` in
    /// (sequence, event ID) order, and nothing after it.
    pub fn ack_notifications(&self, through: &NotificationCursor) -> Result<u64, StoreError> {
        self.ack(
            OutboxStream::Notification,
            through.sequence,
            "(sequence < ? OR (sequence = ? AND event_id <= ?))".to_owned(),
            vec![
                uint(through.sequence)?,
                uint(through.sequence)?,
                through.event_id.as_str().into(),
            ],
        )
    }

    fn ack(
        &self,
        stream: OutboxStream,
        through: u64,
        clause: String,
        params: Vec<Value>,
    ) -> Result<u64, StoreError> {
        self.write(move |store| {
            let head = store.own_head()?.ok_or(StoreError::NotFound)?;
            if through > head.revision {
                return Err(StoreError::InvalidBatch("outbox.ack_beyond_head"));
            }
            let now = store.sql.now_ms();
            let pending = store.sql.query(
                &format!(
                    "SELECT COUNT(*) FROM {} WHERE acked_at_ms IS NULL AND {clause}",
                    stream.table()
                ),
                &params,
            )?;
            let count = pending
                .first()
                .ok_or(StoreError::Backend("do.decode"))?
                .unsigned(0)?;
            let mut update = vec![int(now)?];
            update.extend(params);
            store.sql.exec(
                &format!(
                    "UPDATE {} SET acked_at_ms = ? WHERE acked_at_ms IS NULL AND {clause}",
                    stream.table()
                ),
                &update,
            )?;
            if count > 0 {
                store.sql.exec(
                    "DELETE FROM outbox_retry WHERE stream = ?",
                    &[stream.as_str().into()],
                )?;
            }
            Ok(count)
        })
    }

    /// When the stream's undelivered entries next need delivering (its
    /// retry time); `None` when nothing in it is pending.
    pub fn outbox_retry_at(&self, stream: OutboxStream) -> Result<Option<i64>, StoreError> {
        if self.own_head()?.is_none() {
            return Err(StoreError::NotFound);
        }
        retry_at(self, stream)
    }

    /// Record a failed delivery attempt: the stream's next alarm backs off
    /// (10 s, 20 s, 40 s, ... up to 10 min). Returns the next attempt time.
    pub fn defer_outbox(&self, stream: OutboxStream) -> Result<i64, StoreError> {
        self.write(move |store| {
            let head = store.own_head()?.ok_or(StoreError::NotFound)?;
            let now = store.sql.now_ms();
            let attempts = store
                .sql
                .query(
                    "SELECT attempts FROM outbox_retry WHERE stream = ?",
                    &[stream.as_str().into()],
                )?
                .first()
                .map(|row| row.unsigned(0))
                .transpose()?
                .unwrap_or(0)
                .saturating_add(1);
            let exponent = u32::try_from(attempts.saturating_sub(1))
                .unwrap_or(u32::MAX)
                .min(16);
            let delay = OUTBOX_FIRST_RETRY_MS
                .saturating_mul(1_i64 << exponent)
                .min(OUTBOX_MAX_RETRY_MS);
            let next = now.saturating_add(delay);
            store.sql.exec(
                "INSERT INTO outbox_retry (org_id, project_id, execution_id, stream, attempts, \
                 next_attempt_at_ms) VALUES (?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (stream) DO UPDATE SET attempts = excluded.attempts, \
                 next_attempt_at_ms = excluded.next_attempt_at_ms",
                &store.owned(
                    &head.execution_id,
                    vec![stream.as_str().into(), uint(attempts)?, int(next)?],
                ),
            )?;
            Ok(next)
        })
    }
}
