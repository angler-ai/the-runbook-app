//! B2a.3 WorkQueue on PostgreSQL. Due work derives from the authoritative
//! timer and outbox rows, exactly as the memory store derives it; the
//! work_lease table owns only lease generations, deadlines and acks.
use crate::{
    PostgresExecutionStore, catalog,
    execution::READ_COMMITTED,
    rows::{db, decode, encode, int, uint},
};
use runbook_core::{
    StoreError, TimerRecord,
    catalog::TenantScope,
    work_queue::{WorkId, WorkKind, WorkLease, WorkQueue},
};
use runbook_engine::NotificationIntent;
use tokio_postgres::Client;

const NOW_MS: &str = "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint";
async fn now_ms(c: &Client) -> Result<i64, StoreError> {
    c.query_one(NOW_MS, &[])
        .await
        .map(|row| row.get(0))
        .map_err(db)
}

/// Lease primary-key parts for one work item (notification generation is 0).
pub(crate) fn columns(id: &WorkId) -> (&'static str, &str, u64) {
    match &id.kind {
        WorkKind::Timer { key, generation } => ("timer", key.as_str(), *generation),
        WorkKind::Notification { event_id } => ("notification", event_id.as_str(), 0),
        WorkKind::Activation => ("activation", "", 0),
        WorkKind::ClaimLease {
            step_run_id,
            generation,
        } => ("claim", step_run_id.as_str(), *generation),
    }
}

/// Due timers and notifications for this tenant, in the memory store's
/// (due time, work ID) claim order.
async fn due_items(
    c: &Client,
    scope: &TenantScope,
    now: i64,
) -> Result<Vec<(i64, WorkId)>, StoreError> {
    let mut due = vec![];
    for row in c
        .query(
            // LANG4: a call work item, once acknowledged, is never due
            // again; its row stays (the run's timer history) but is not read.
            "SELECT t.body FROM runbook.execution_timer t \
             JOIN runbook.execution_owner o USING (execution_id) \
             WHERE o.org_id = $1 AND o.project_id = $2 \
             AND NOT (t.key LIKE 'call-%' AND EXISTS (\
               SELECT 1 FROM runbook.work_lease l \
               WHERE l.org_id = o.org_id AND l.project_id = o.project_id \
               AND l.execution_id = t.execution_id AND l.kind = 'timer' \
               AND l.work_key = t.key \
               AND l.work_generation = (t.body::jsonb->>'generation')::bigint \
               AND l.acknowledged))",
            &[&scope.org_id.as_str(), &scope.project_id.as_str()],
        )
        .await
        .map_err(db)?
    {
        let timer: TimerRecord = decode(row.get(0))?;
        if timer.due_at_ms <= now {
            due.push((
                timer.due_at_ms,
                WorkId {
                    execution_id: timer.execution_id.clone(),
                    kind: WorkKind::Timer {
                        key: timer.key.clone(),
                        generation: timer.generation,
                    },
                },
            ));
        }
    }
    for row in c
        .query(
            "SELECT b.body FROM runbook.execution_outbox b \
             JOIN runbook.execution_owner o USING (execution_id) \
             WHERE o.org_id = $1 AND o.project_id = $2",
            &[&scope.org_id.as_str(), &scope.project_id.as_str()],
        )
        .await
        .map_err(db)?
    {
        let intent: NotificationIntent = decode(row.get(0))?;
        if intent.at_ms <= now {
            due.push((
                intent.at_ms,
                WorkId {
                    execution_id: intent.execution_id.clone(),
                    kind: WorkKind::Notification {
                        event_id: intent.event_id.clone(),
                    },
                },
            ));
        }
    }
    for row in c
        .query(
            "SELECT s.execution_id, COALESCE(a.admitted_at_ms, r.created_at_ms) \
             FROM runbook.execution_start s \
             JOIN runbook.execution_owner o USING (execution_id) \
             JOIN runbook.execution e USING (execution_id) \
             LEFT JOIN runbook.admission a USING (execution_id) \
             LEFT JOIN runbook.run_summary r USING (execution_id) \
             WHERE o.org_id = $1 AND o.project_id = $2 AND e.revision = 0 \
             AND (a.execution_id IS NULL \
                  OR (a.state = 'admitted' AND a.admitted_at_ms IS NOT NULL)) \
             AND COALESCE(a.admitted_at_ms, r.created_at_ms) <= $3",
            &[&scope.org_id.as_str(), &scope.project_id.as_str(), &now],
        )
        .await
        .map_err(db)?
    {
        due.push((
            row.get(1),
            WorkId {
                execution_id: row
                    .get::<_, &str>(0)
                    .parse()
                    .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?,
                kind: WorkKind::Activation,
            },
        ));
    }
    for row in c
        .query(
            "SELECT cl.execution_id, cl.step_run_id, cl.generation, cl.lease_deadline_at_ms \
             FROM runbook.execution_claim cl \
             JOIN runbook.execution_owner o USING (execution_id) \
             WHERE o.org_id = $1 AND o.project_id = $2 AND NOT cl.released \
             AND cl.lease_deadline_at_ms IS NOT NULL AND cl.lease_deadline_at_ms <= $3",
            &[&scope.org_id.as_str(), &scope.project_id.as_str(), &now],
        )
        .await
        .map_err(db)?
    {
        due.push((
            row.get(3),
            WorkId {
                execution_id: row
                    .get::<_, &str>(0)
                    .parse()
                    .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?,
                kind: WorkKind::ClaimLease {
                    step_run_id: row
                        .get::<_, &str>(1)
                        .parse()
                        .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?,
                    generation: uint(row.get(2))?,
                },
            },
        ));
    }
    due.sort();
    Ok(due)
}

impl WorkQueue for PostgresExecutionStore {
    async fn claim_due(&self, limit: usize, lease_ms: u64) -> Result<Vec<WorkLease>, StoreError> {
        if lease_ms == 0 {
            return Err(StoreError::InvalidBatch("queue.invalid_lease"));
        }
        self.transaction(READ_COMMITTED, async |c| {
            let now = now_ms(c).await?;
            let deadline = now
                .checked_add(
                    i64::try_from(lease_ms)
                        .map_err(|_| StoreError::InvalidBatch("queue.lease_overflow"))?,
                )
                .ok_or(StoreError::InvalidBatch("queue.lease_overflow"))?;
            let mut claimed = vec![];
            for (_, id) in due_items(c, &self.scope, now).await? {
                if claimed.len() == limit {
                    break;
                }
                let (kind, key, work_generation) = columns(&id);
                let lease_key: [&(dyn tokio_postgres::types::ToSql + Sync); 6] = [
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &id.execution_id.as_str(),
                    &kind,
                    &key,
                    &int(work_generation)?,
                ];
                let first = WorkLease {
                    scope: self.scope.clone(),
                    id: id.clone(),
                    generation: 1,
                    deadline_at_ms: deadline,
                };
                // A fresh item is claimed by inserting its lease row; a
                // concurrent claim of the same item conflicts and skips.
                let inserted = c
                    .execute(
                        "INSERT INTO runbook.work_lease \
                         (org_id, project_id, execution_id, kind, work_key, work_generation, \
                          generation, deadline_at_ms, acknowledged, body) \
                         VALUES ($1, $2, $3, $4, $5, $6, 1, $7, false, $8) \
                         ON CONFLICT DO NOTHING",
                        &[
                            &self.scope.org_id.as_str(),
                            &self.scope.project_id.as_str(),
                            &id.execution_id.as_str(),
                            &kind,
                            &key,
                            &int(work_generation)?,
                            &deadline,
                            &encode(&first)?,
                        ],
                    )
                    .await
                    .map_err(db)?;
                if inserted == 1 {
                    claimed.push(first);
                    continue;
                }
                // An existing lease is reclaimed only under its row lock;
                // a worker already holding it makes this claim skip.
                let Some(row) = c
                    .query_opt(
                        "SELECT generation, deadline_at_ms, acknowledged \
                         FROM runbook.work_lease \
                         WHERE org_id = $1 AND project_id = $2 AND execution_id = $3 \
                         AND kind = $4 AND work_key = $5 AND work_generation = $6 \
                         FOR UPDATE SKIP LOCKED",
                        &lease_key,
                    )
                    .await
                    .map_err(db)?
                else {
                    continue;
                };
                let (generation, leased_until, acknowledged): (i64, i64, bool) =
                    (row.get(0), row.get(1), row.get(2));
                if acknowledged || leased_until > now {
                    continue;
                }
                let generation = uint(generation)?
                    .checked_add(1)
                    .ok_or(StoreError::InvalidBatch("queue.generation_overflow"))?;
                let lease = WorkLease {
                    scope: self.scope.clone(),
                    id: id.clone(),
                    generation,
                    deadline_at_ms: deadline,
                };
                c.execute(
                    "UPDATE runbook.work_lease \
                     SET generation = $7, deadline_at_ms = $8, body = $9 \
                     WHERE org_id = $1 AND project_id = $2 AND execution_id = $3 \
                     AND kind = $4 AND work_key = $5 AND work_generation = $6",
                    &[
                        &self.scope.org_id.as_str(),
                        &self.scope.project_id.as_str(),
                        &lease.id.execution_id.as_str(),
                        &kind,
                        &key,
                        &int(work_generation)?,
                        &int(generation)?,
                        &deadline,
                        &encode(&lease)?,
                    ],
                )
                .await
                .map_err(db)?;
                claimed.push(lease);
            }
            Ok(claimed)
        })
        .await
    }

    async fn ack(&self, id: &WorkId, generation: u64) -> Result<(), StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            if catalog::owner(c, &id.execution_id).await?.as_ref() != Some(&self.scope) {
                return Err(StoreError::NotFound);
            }
            let now = now_ms(c).await?;
            let activated = match &id.kind {
                WorkKind::Activation => c
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM runbook.execution \
                         WHERE execution_id = $1 AND revision > 0)",
                        &[&id.execution_id.as_str()],
                    )
                    .await
                    .map_err(db)?
                    .get(0),
                WorkKind::ClaimLease { step_run_id, .. } => c
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM runbook.execution_claim \
                         WHERE execution_id = $1 AND step_run_id = $2 AND released)",
                        &[&id.execution_id.as_str(), &step_run_id.as_str()],
                    )
                    .await
                    .map_err(db)?
                    .get(0),
                _ => false,
            };
            if !activated
                && !due_items(c, &self.scope, now)
                    .await?
                    .iter()
                    .any(|(_, item)| item == id)
            {
                return Err(StoreError::InvalidBatch("queue.work_obsolete"));
            }
            let (kind, key, work_generation) = columns(id);
            let row = c
                .query_opt(
                    "SELECT generation, deadline_at_ms, acknowledged \
                     FROM runbook.work_lease \
                     WHERE org_id = $1 AND project_id = $2 AND execution_id = $3 \
                     AND kind = $4 AND work_key = $5 AND work_generation = $6 FOR UPDATE",
                    &[
                        &self.scope.org_id.as_str(),
                        &self.scope.project_id.as_str(),
                        &id.execution_id.as_str(),
                        &kind,
                        &key,
                        &int(work_generation)?,
                    ],
                )
                .await
                .map_err(db)?
                .ok_or(StoreError::NotFound)?;
            let (leased, leased_until, acknowledged): (i64, i64, bool) =
                (row.get(0), row.get(1), row.get(2));
            if acknowledged || uint(leased)? != generation || leased_until <= now {
                return Err(StoreError::InvalidBatch("queue.stale_lease"));
            }
            c.execute(
                "UPDATE runbook.work_lease SET acknowledged = true \
                 WHERE org_id = $1 AND project_id = $2 AND execution_id = $3 \
                 AND kind = $4 AND work_key = $5 AND work_generation = $6",
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &id.execution_id.as_str(),
                    &kind,
                    &key,
                    &int(work_generation)?,
                ],
            )
            .await
            .map_err(db)?;
            Ok(())
        })
        .await
    }
}

impl PostgresExecutionStore {
    /// HOST1: give back a lease this worker claimed but will not process (it
    /// is shutting down), so another instance can claim the item at once
    /// instead of when the lease would have expired. Only the holder's
    /// generation is touched; an acknowledged or reclaimed lease is left as
    /// it is. Returns whether the lease was released.
    pub async fn release_lease(&self, lease: &WorkLease) -> Result<bool, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            let now = now_ms(c).await?;
            let (kind, key, work_generation) = columns(&lease.id);
            let released = c
                .execute(
                    "UPDATE runbook.work_lease SET deadline_at_ms = $8 \
                     WHERE org_id = $1 AND project_id = $2 AND execution_id = $3 \
                     AND kind = $4 AND work_key = $5 AND work_generation = $6 \
                     AND generation = $7 AND NOT acknowledged AND deadline_at_ms > $8",
                    &[
                        &self.scope.org_id.as_str(),
                        &self.scope.project_id.as_str(),
                        &lease.id.execution_id.as_str(),
                        &kind,
                        &key,
                        &int(work_generation)?,
                        &int(lease.generation)?,
                        &now,
                    ],
                )
                .await
                .map_err(db)?;
            Ok(released == 1)
        })
        .await
    }
}
