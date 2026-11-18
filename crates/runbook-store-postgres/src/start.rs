//! B5a start commits on PostgreSQL: replay check, owner claim, head,
//! start record, pooled admission, derived summary and replay save in
//! one transaction.
use crate::{
    PostgresExecutionStore, catalog,
    execution::{READ_COMMITTED, finish, insert_head},
    retention::tombstoned,
    rows::{connection, db, decode, encode},
};
use runbook_core::{
    StoreError,
    start::{StartCommit, StartOutcome, StartRecord, StartStore},
};
use runbook_model::ExecutionId;

async fn read_replay(
    c: &tokio_postgres::Client,
    scope: &runbook_core::catalog::TenantScope,
    key: &str,
    now: i64,
) -> Result<Option<(runbook_core::start::StartReplay, i64)>, StoreError> {
    let row = c
        .query_opt(
            "SELECT principal, request_hash, outcome, created_at_ms \
             FROM runbook.start_replay \
             WHERE org_id = $1 AND project_id = $2 AND idempotency_key = $3",
            &[&scope.org_id.as_str(), &scope.project_id.as_str(), &key],
        )
        .await
        .map_err(db)?;
    let Some(row) = row else { return Ok(None) };
    let created: i64 = row.get(3);
    if now - created >= runbook_core::start::REPLAY_RETENTION_MS {
        c.execute(
            "DELETE FROM runbook.start_replay \
             WHERE org_id = $1 AND project_id = $2 AND idempotency_key = $3",
            &[&scope.org_id.as_str(), &scope.project_id.as_str(), &key],
        )
        .await
        .map_err(db)?;
        return Ok(None);
    }
    let outcome: runbook_core::start::StartOutcome = decode(row.get(2))?;
    Ok(Some((
        runbook_core::start::StartReplay {
            key: key.to_owned(),
            principal: row.get(0),
            request_hash: row
                .get::<_, Vec<u8>>(1)
                .try_into()
                .map_err(|_| StoreError::Backend("postgres.invalid_hash"))?,
            outcome,
        },
        created,
    )))
}
async fn clock_ms(c: &tokio_postgres::Client) -> Result<i64, StoreError> {
    c.query_one(
        "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
        &[],
    )
    .await
    .map(|row| row.get(0))
    .map_err(db)
}

impl PostgresExecutionStore {
    /// LANG3, inside the start transaction. Lock order is B2.2a's:
    /// execution heads (the runs to cancel, by ID), then the pool, then its
    /// admission rows. The core decides (`concurrency::resolve`); on a
    /// preemption this releases the evicted waiters and applies the
    /// prepared cancellations as fenced CAS commits (an evicted holder keeps
    /// its slot until it closes), and answers that the start goes first in
    /// line. Any mismatch rolls the whole start back as
    /// `start.preemption_stale`.
    async fn resolve_conflict(
        &self,
        c: &tokio_postgres::Client,
        commit: &StartCommit,
        request: &runbook_core::catalog::AdmitRequest,
    ) -> Result<bool, StoreError> {
        use runbook_core::concurrency::{OnConflict, Resolution, STALE_CODE, resolve};
        let Some(on_conflict) = &commit.on_conflict else {
            return Ok(false);
        };
        let mut heads = std::collections::BTreeMap::new();
        if let OnConflict::CancelPrevious { commits, .. } = on_conflict {
            let mut ids: Vec<&ExecutionId> = commits.iter().map(|b| &b.execution_id).collect();
            ids.sort();
            for id in ids {
                let head = self.lock_head(c, id).await?;
                heads.insert(id.clone(), head);
            }
        }
        catalog::lock_pool(c, &self.scope, request).await?;
        let view = catalog::admission_pool(c, &self.scope, &request.key, true).await?;
        let Resolution::Preempt { waiters } = resolve(Some(on_conflict), request.limit, &view)?
        else {
            return Ok(false);
        };
        for waiter in &waiters {
            catalog::release_admission(c, &self.scope, waiter).await?;
            // An evicted waiter was cancelled while queued: it never counts.
            refund_quota(c, waiter).await?;
        }
        if let OnConflict::CancelPrevious { commits, .. } = on_conflict {
            for batch in commits {
                let head = heads.remove(&batch.execution_id).flatten();
                if self.apply_commit(c, batch.clone(), head, None).await?
                    == runbook_core::CommitResult::Conflict
                {
                    return Err(StoreError::InvalidBatch(STALE_CODE));
                }
            }
        }
        Ok(true)
    }
}

impl StartStore for PostgresExecutionStore {
    async fn commit_start(&self, commit: StartCommit) -> Result<StartOutcome, StoreError> {
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(READ_COMMITTED).await.map_err(db)?;
        let result = async {
            let c = conn.client();
            let id = &commit.projection.execution_id;
            if let Some(replay) = &commit.replay {
                // Serialize same-key starts so a racing retry reads the
                // saved outcome instead of starting twice.
                c.query_one(
                    "SELECT pg_advisory_xact_lock(\
                     hashtextextended($1 || '/' || $2 || '/start/' || $3, 0))",
                    &[
                        &self.scope.org_id.as_str(),
                        &self.scope.project_id.as_str(),
                        &replay.key,
                    ],
                )
                .await
                .map_err(db)?;
                if let Some((saved, _)) =
                    read_replay(c, &self.scope, &replay.key, clock_ms(c).await?).await?
                {
                    if saved.principal != replay.principal
                        || saved.request_hash != replay.request_hash
                    {
                        return Err(StoreError::InvalidBatch(
                            "request.idempotency_key_reused",
                        ));
                    }
                    return Ok(saved.outcome);
                }
            }
            if tombstoned(c, id).await? {
                return Err(StoreError::NotFound);
            }
            match catalog::claim_owner(c, &self.scope, id).await {
                Err(StoreError::InvalidBatch("catalog.execution_scope_conflict")) => {
                    return Err(StoreError::NotFound);
                }
                other => other?,
            }
            // CF6: count the start in its month (refused past the limit; the
            // transaction rolls the count back if anything later fails).
            reserve_quota(
                c,
                &self.quota,
                &self.scope,
                id,
                clock_ms(c).await? + self.quota_clock_offset_ms,
            )
            .await?;
            if !insert_head(c, &commit.projection).await? {
                return Err(StoreError::InvalidBatch("store.execution_exists"));
            }
            c.execute(
                "INSERT INTO runbook.execution_start (execution_id, org_id, project_id, body) \
                 VALUES ($1, $2, $3, $4)",
                &[
                    &id.as_str(),
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &encode(&commit.record)?,
                ],
            )
            .await
            .map_err(db)?;
            let now: i64 = c
                .query_one(
                    "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                    &[],
                )
                .await
                .map_err(db)?
                .get(0);
            let admission = match &commit.admission {
                Some(request) => {
                    let first = self.resolve_conflict(c, &commit, request).await?;
                    Some(
                        catalog::admit_pooled_at(c, &self.scope, request.clone(), now, first)
                            .await?,
                    )
                }
                None => None,
            };
            // C1: a queued start's `execution.queued` intent commits with it.
            if let Some(intent) = runbook_core::start::queued_outbox(
                id,
                &commit.record,
                admission.as_ref(),
                now,
            ) {
                c.execute(
                    "INSERT INTO runbook.execution_outbox (execution_id, event_id, body) \
                     VALUES ($1, $2, $3)",
                    &[&id.as_str(), &intent.event_id, &encode(&intent)?],
                )
                .await
                .map_err(db)?;
            }
            let mut derived = commit.derived.clone();
            derived.summary.created_at_ms = now;
            catalog::apply_derived(c, &self.scope, derived).await?;
            let outcome = StartOutcome {
                execution_id: id.clone(),
                runbook_version_id: commit.record.version_id.clone(),
                runbook_version: commit.record.version,
                admission,
            };
            if let Some(replay) = &commit.replay {
                c.execute(
                    "INSERT INTO runbook.start_replay \
                     (org_id, project_id, idempotency_key, principal, request_hash, execution_id, outcome, created_at_ms) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                    &[
                        &self.scope.org_id.as_str(),
                        &self.scope.project_id.as_str(),
                        &replay.key,
                        &replay.principal,
                        &replay.request_hash.as_slice(),
                        &id.as_str(),
                        &encode(&outcome)?,
                        &now,
                    ],
                )
                .await
                .map_err(db)?;
            }
            Ok(outcome)
        }
        .await;
        finish(&mut conn, result).await
    }

    async fn start_record(&self, id: &ExecutionId) -> Result<Option<StartRecord>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT body FROM runbook.execution_start \
                 WHERE org_id = $1 AND project_id = $2 AND execution_id = $3",
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &id.as_str(),
                ],
            )
            .await
            .map_err(db)?
            .map(|row| decode(row.get(0)))
            .transpose()
    }

    async fn find_replay(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<runbook_core::start::StartReplay>, StoreError> {
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(READ_COMMITTED).await.map_err(db)?;
        let result = async {
            let now = clock_ms(conn.client()).await?;
            Ok(
                read_replay(conn.client(), &self.scope, idempotency_key, now)
                    .await?
                    .map(|(replay, _)| replay),
            )
        }
        .await;
        finish(&mut conn, result).await
    }

    async fn cancel_queued(
        &self,
        id: &ExecutionId,
        replay: Option<&runbook_core::actor::Replay>,
    ) -> Result<bool, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            if catalog::owner(c, id).await?.as_ref() != Some(&self.scope) {
                return Err(StoreError::NotFound);
            }
            self.lock_head(c, id).await?.ok_or(StoreError::NotFound)?;
            let now: i64 = c
                .query_one(
                    "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                    &[],
                )
                .await
                .map_err(db)?
                .get(0);
            if let Some(value) =
                crate::claims::replay_lookup(c, id, replay, "cancel_queued", now).await?
            {
                return Ok(value);
            }
            // F37: release the row rather than delete it. Activation treats a
            // run with no admission row as due, so a deleted row would let the
            // cancelled run start; a released, unqueued row is never promoted.
            let deleted = c
                .execute(
                    "UPDATE runbook.admission SET state = 'released', queue_order = NULL \
                     WHERE execution_id = $1 AND state = 'queued'",
                    &[&id.as_str()],
                )
                .await
                .map_err(db)?;
            if deleted == 1 {
                // CF6: a run cancelled while queued never counts.
                refund_quota(c, id).await?;
            }
            crate::claims::replay_write(c, id, replay, "cancel_queued", now, deleted == 1).await?;
            self.checkpoint(runbook_core::WriteStage::Idempotency)?;
            Ok(deleted == 1)
        })
        .await
    }
}

/// CF6: count `id`'s start in its organization's month, refusing it past
/// `limits` (the upsert's row lock serializes concurrent starts).
async fn reserve_quota(
    c: &tokio_postgres::Client,
    limits: &runbook_core::quotas::QuotaLimits,
    scope: &runbook_core::catalog::TenantScope,
    id: &ExecutionId,
    now: i64,
) -> Result<(), StoreError> {
    let month = runbook_core::quotas::month_of(now);
    let cap: Option<i64> = limits.executions_monthly.map(i64::from);
    let counted = c
        .query_opt(
            "INSERT INTO runbook.usage_monthly (org_id, month, executions_started) \
             VALUES ($1, $2, 1) ON CONFLICT (org_id, month) DO UPDATE \
             SET executions_started = runbook.usage_monthly.executions_started + 1 \
             WHERE $3::bigint IS NULL OR runbook.usage_monthly.executions_started < $3 \
             RETURNING executions_started",
            &[&scope.org_id.as_str(), &month, &cap],
        )
        .await
        .map_err(db)?;
    if counted.is_none() {
        return Err(StoreError::InvalidBatch(runbook_core::quotas::MONTHLY_CODE));
    }
    c.execute(
        "INSERT INTO runbook.quota_reservation (execution_id, org_id, month, created_at_ms) \
         VALUES ($1, $2, $3, $4)",
        &[&id.as_str(), &scope.org_id.as_str(), &month, &now],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// CF6: give back a start's count (a run cancelled while queued).
pub(crate) async fn refund_quota(
    c: &tokio_postgres::Client,
    id: &ExecutionId,
) -> Result<(), StoreError> {
    c.execute(
        "WITH r AS (DELETE FROM runbook.quota_reservation WHERE execution_id = $1 \
           RETURNING org_id, month) \
         UPDATE runbook.usage_monthly u SET executions_started = u.executions_started - 1 \
         FROM r WHERE u.org_id = r.org_id AND u.month = r.month AND u.executions_started > 0",
        &[&id.as_str()],
    )
    .await
    .map_err(db)?;
    Ok(())
}

impl runbook_core::quotas::QuotaStore for PostgresExecutionStore {
    async fn org_usage(
        &self,
        org: &runbook_model::OrganizationId,
    ) -> Result<runbook_core::quotas::OrgUsage, StoreError> {
        if org != &self.scope.org_id {
            return Err(StoreError::NotFound);
        }
        let conn = self.pool.get().await.map_err(connection)?;
        let c = conn.client();
        let month = runbook_core::quotas::month_of(clock_ms(c).await? + self.quota_clock_offset_ms);
        let row = c
            .query_one(
                "SELECT COALESCE((SELECT executions_started FROM runbook.usage_monthly \
                   WHERE org_id = $1 AND month = $2), 0), \
                 (SELECT count(*) FROM runbook.execution e JOIN runbook.execution_owner o \
                   ON o.execution_id = e.execution_id WHERE o.org_id = $1 \
                   AND e.status NOT IN ('queued', 'succeeded', 'failed', 'cancelled'))",
                &[&org.as_str(), &month],
            )
            .await
            .map_err(db)?;
        Ok(runbook_core::quotas::OrgUsage {
            month,
            executions_started: u64::try_from(row.get::<_, i64>(0)).unwrap_or(0),
            running: u64::try_from(row.get::<_, i64>(1)).unwrap_or(0),
            limits: self.quota,
        })
    }
}
