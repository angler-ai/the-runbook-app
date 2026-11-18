//! `WorkQueue` over this execution's own rows, and the alarm mapping.
//!
//! Due work derives from the authoritative rows, exactly as the native
//! adapters derive it: live timers, undelivered notification intents, the
//! D24 activation of an admitted, unstarted run, and expired agent claim
//! leases. `work_lease` owns only lease generations, deadlines and acks.
//!
//! **Alarm mapping.** A DO has one alarm, so every write transaction ends
//! by setting it to [`next_alarm_at`]: the earliest moment any work here
//! needs the object awake. That is the earliest of
//! - each unacknowledged timer, claim deadline or activation, at its due
//!   time, or at its lease's deadline while a worker holds it (a crashed
//!   worker's lease then wakes the object to retry);
//! - each outbox stream with undelivered entries, at its retry time
//!   ([`crate::outbox`]), but never sooner than [`OUTBOX_MIN_RETRY_MS`]
//!   from now; the host delivers right after commit, so the alarm is only
//!   the backstop.
//!
//! Every time that can reach the alarm (timers, claim deadlines, admission)
//! is refused beyond [`MAX_ALARM_MS`], the range of a JavaScript `Date`, so
//! `setAlarm` cannot throw and wedge every later write.
//!
//! Setting the alarm inside the transaction makes it roll back with the
//! rows (SQLite-backed objects store it transactionally; CF1 proved it).
use crate::{
    DoSql, DoStore,
    outbox::{OUTBOX_MIN_RETRY_MS, OutboxStream, retry_at},
    sql::{Columns, Value, int, uint},
};
use runbook_core::{
    StoreError,
    work_queue::{WorkId, WorkKind, WorkLease, WorkQueue},
};
use runbook_model::ExecutionId;

pub(crate) fn columns(kind: &WorkKind) -> Result<(&'static str, String, Value), StoreError> {
    Ok(match kind {
        WorkKind::Timer { key, generation } => ("timer", key.clone(), uint(*generation)?),
        WorkKind::Notification { event_id } => ("notification", event_id.clone(), uint(0)?),
        WorkKind::Activation => ("activation", String::new(), uint(0)?),
        WorkKind::ClaimLease {
            step_run_id,
            generation,
        } => ("claim", step_run_id.as_str().to_owned(), uint(*generation)?),
    })
}

/// One lease row: (generation, deadline, acknowledged).
struct LeaseRow {
    generation: u64,
    deadline_at_ms: i64,
    acknowledged: bool,
}

impl<B: DoSql> DoStore<B> {
    fn lease_row(&self, kind: &WorkKind) -> Result<Option<LeaseRow>, StoreError> {
        let (kind, key, generation) = columns(kind)?;
        self.sql
            .query(
                "SELECT generation, deadline_at_ms, acknowledged FROM work_lease \
                 WHERE kind = ? AND work_key = ? AND work_generation = ?",
                &[kind.into(), key.into(), generation],
            )?
            .first()
            .map(|row| {
                Ok(LeaseRow {
                    generation: row.unsigned(0)?,
                    deadline_at_ms: row.integer(1)?,
                    acknowledged: row.flag(2)?,
                })
            })
            .transpose()
    }

    /// Every work item with the time it becomes due, due or not yet.
    fn work_items(&self, id: &ExecutionId) -> Result<Vec<(i64, WorkKind)>, StoreError> {
        let mut items = vec![];
        for row in self
            .sql
            .query("SELECT key, generation, due_at_ms FROM timer", &[])?
        {
            let kind = WorkKind::Timer {
                key: row.text(0)?.to_owned(),
                generation: row.unsigned(1)?,
            };
            // LANG4: a call item, once done, is no longer listed.
            if let WorkKind::Timer { key, .. } = &kind
                && runbook_core::calls::is_call_key(key)
                && self.lease_row(&kind)?.is_some_and(|l| l.acknowledged)
            {
                continue;
            }
            items.push((row.integer(2)?, kind));
        }
        for row in self.sql.query(
            "SELECT event_id, at_ms FROM outbox WHERE acked_at_ms IS NULL",
            &[],
        )? {
            items.push((
                row.integer(1)?,
                WorkKind::Notification {
                    event_id: row.text(0)?.to_owned(),
                },
            ));
        }
        for row in self.sql.query(
            "SELECT s.activation_at_ms FROM start_record s JOIN meta m ON m.id = 1 \
             WHERE m.revision = 0 AND s.activation_at_ms IS NOT NULL",
            &[],
        )? {
            items.push((row.integer(0)?, WorkKind::Activation));
        }
        for row in self.sql.query(
            "SELECT step_run_id, generation, lease_deadline_at_ms FROM claim \
             WHERE released = 0 AND lease_deadline_at_ms IS NOT NULL",
            &[],
        )? {
            items.push((
                row.integer(2)?,
                WorkKind::ClaimLease {
                    step_run_id: row
                        .text(0)?
                        .parse()
                        .map_err(|_| StoreError::Backend("do.invalid_stored_id"))?,
                    generation: row.unsigned(1)?,
                },
            ));
        }
        let _ = id;
        items.sort();
        Ok(items)
    }

    fn due_items(&self, id: &ExecutionId, now: i64) -> Result<Vec<WorkKind>, StoreError> {
        Ok(self
            .work_items(id)?
            .into_iter()
            .filter(|(due, _)| *due <= now)
            .map(|(_, kind)| kind)
            .collect())
    }

    /// Lease due work of the kinds `wanted` accepts, and nothing else: a host
    /// claims only what it will act on, so other work stays free for its
    /// own consumer instead of waiting out a lease nobody works.
    pub fn claim_due_of(
        &self,
        limit: usize,
        lease_ms: u64,
        wanted: impl Fn(&WorkKind) -> bool + 'static,
    ) -> Result<Vec<WorkLease>, StoreError> {
        if lease_ms == 0 {
            return Err(StoreError::InvalidBatch("queue.invalid_lease"));
        }
        let lease_ms = i64::try_from(lease_ms)
            .map_err(|_| StoreError::InvalidBatch("queue.lease_overflow"))?;
        self.write(move |store| {
            // Another tenant's client sees no work here at all.
            let Some(head) = store.own_head()? else {
                return Ok(vec![]);
            };
            let now = store.sql.now_ms();
            let deadline = alarm_time(
                now.checked_add(lease_ms)
                    .ok_or(StoreError::InvalidBatch("queue.lease_overflow"))?,
                "queue.lease_overflow",
            )?;
            let mut claimed = vec![];
            for kind in store.due_items(&head.execution_id, now)? {
                if claimed.len() == limit {
                    break;
                }
                if !wanted(&kind) {
                    continue;
                }
                let generation = match store.lease_row(&kind)? {
                    None => 1,
                    Some(row) if row.acknowledged || row.deadline_at_ms > now => continue,
                    Some(row) => row
                        .generation
                        .checked_add(1)
                        .ok_or(StoreError::InvalidBatch("queue.generation_overflow"))?,
                };
                let (name, key, work_generation) = columns(&kind)?;
                store.sql.exec(
                    "INSERT INTO work_lease (org_id, project_id, execution_id, kind, work_key, \
                     work_generation, generation, deadline_at_ms, acknowledged) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0) \
                     ON CONFLICT (kind, work_key, work_generation) DO UPDATE SET \
                     generation = excluded.generation, deadline_at_ms = excluded.deadline_at_ms",
                    &store.owned(
                        &head.execution_id,
                        vec![
                            name.into(),
                            key.into(),
                            work_generation,
                            uint(generation)?,
                            int(deadline)?,
                        ],
                    ),
                )?;
                claimed.push(WorkLease {
                    scope: store.scope.clone(),
                    id: WorkId {
                        execution_id: head.execution_id.clone(),
                        kind,
                    },
                    generation,
                    deadline_at_ms: deadline,
                });
            }
            Ok(claimed)
        })
    }

    /// Test seam: expire one lease now, as a crashed worker's would.
    #[doc(hidden)]
    pub fn expire_lease(&self, lease: &WorkLease) -> Result<(), StoreError> {
        let (kind, key, generation) = columns(&lease.id.kind)?;
        self.write(move |store| {
            store.sql.exec(
                "UPDATE work_lease SET deadline_at_ms = 0 \
                 WHERE kind = ? AND work_key = ? AND work_generation = ?",
                &[kind.into(), key.into(), generation],
            )
        })
    }
}

impl<B: DoSql> WorkQueue for DoStore<B> {
    async fn claim_due(&self, limit: usize, lease_ms: u64) -> Result<Vec<WorkLease>, StoreError> {
        self.claim_due_of(limit, lease_ms, |_| true)
    }

    async fn ack(&self, id: &WorkId, generation: u64) -> Result<(), StoreError> {
        let id = id.clone();
        self.write(move |store| {
            let head = store.owned_head(&id.execution_id)?;
            let now = store.sql.now_ms();
            let done = match &id.kind {
                WorkKind::Activation => head.revision > 0,
                WorkKind::ClaimLease { step_run_id, .. } => !store
                    .sql
                    .query(
                        "SELECT 1 FROM claim WHERE step_run_id = ? AND released = 1",
                        &[step_run_id.as_str().into()],
                    )?
                    .is_empty(),
                _ => false,
            };
            if !done && !store.due_items(&head.execution_id, now)?.contains(&id.kind) {
                return Err(StoreError::InvalidBatch("queue.work_obsolete"));
            }
            let row = store.lease_row(&id.kind)?.ok_or(StoreError::NotFound)?;
            if row.acknowledged || row.generation != generation || row.deadline_at_ms <= now {
                return Err(StoreError::InvalidBatch("queue.stale_lease"));
            }
            let (name, key, work_generation) = columns(&id.kind)?;
            store.sql.exec(
                "UPDATE work_lease SET acknowledged = 1 \
                 WHERE kind = ? AND work_key = ? AND work_generation = ?",
                &[name.into(), key.clone().into(), work_generation],
            )?;
            if let WorkKind::Notification { .. } = &id.kind {
                // A delivered notification leaves the drain stream too.
                store.sql.exec(
                    "UPDATE outbox SET acked_at_ms = ? WHERE event_id = ? AND acked_at_ms IS NULL",
                    &[int(now)?, key.into()],
                )?;
                store.sql.exec(
                    "DELETE FROM outbox_retry WHERE stream = ?",
                    &[OutboxStream::Notification.as_str().into()],
                )?;
            }
            Ok(())
        })
    }
}

/// The latest time a JavaScript `Date` (and so `setAlarm`) can hold:
/// ±8.64e15 ms. Every time that can reach the alarm is refused beyond it.
pub const MAX_ALARM_MS: i64 = 8_640_000_000_000_000;

/// Refuse a time the alarm cannot hold.
pub(crate) fn alarm_time(at_ms: i64, code: &'static str) -> Result<i64, StoreError> {
    if at_ms.abs() > MAX_ALARM_MS {
        return Err(StoreError::InvalidBatch(code));
    }
    Ok(at_ms)
}

/// Recompute and set the alarm. Runs at the end of every write transaction.
pub(crate) fn rearm<B: DoSql>(store: &DoStore<B>) -> Result<(), StoreError> {
    let at = next_alarm_at(store)?;
    store.sql.set_alarm(at)
}

/// When this object must wake up next (see the module docs), from the rows
/// alone, whoever's handle asks. `None` means nothing is pending.
pub fn next_alarm_at<B: DoSql>(store: &DoStore<B>) -> Result<Option<i64>, StoreError> {
    let Some(head) = store.head()? else {
        return Ok(None);
    };
    // Gate fix D2: a held import wakes for nothing until it is released,
    // except (gate round 2) to expire an hour on if its import never
    // committed.
    if let Some((_, at)) = store.hold()? {
        return Ok(Some(at.saturating_add(crate::portable::HOLD_EXPIRY_MS)));
    }
    let mut next: Option<i64> = None;
    let mut consider = |at: i64| next = Some(next.map_or(at, |n| n.min(at)));
    for (due, kind) in store.work_items(&head.execution_id)? {
        // Notifications wake the object through their stream's retry time.
        if matches!(kind, WorkKind::Notification { .. }) {
            continue;
        }
        match store.lease_row(&kind)? {
            Some(row) if row.acknowledged => {}
            Some(row) => consider(due.max(row.deadline_at_ms)),
            None => consider(due),
        }
    }
    // An outbox retry is never armed sooner than the minimum backoff from
    // now: one already overdue (a write that did not defer it) wakes the
    // object once, a second from now, instead of firing in a loop.
    let floor = store.sql.now_ms().saturating_add(OUTBOX_MIN_RETRY_MS);
    for stream in [OutboxStream::Notification, OutboxStream::Derived] {
        if let Some(at) = retry_at(store, stream)? {
            consider(at.max(floor));
        }
    }
    Ok(next)
}
