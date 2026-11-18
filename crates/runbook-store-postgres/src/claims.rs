//! B6a lease-only claim writes on PostgreSQL: heartbeat and log appends
//! fence on the claim row lock; claim/release/expire ride execution commits.
use crate::{
    PostgresExecutionStore, catalog,
    execution::READ_COMMITTED,
    rows::{db, decode, encode},
};
use runbook_core::{ClaimWrite, StoreError, claims::ClaimStore};
use runbook_model::{ActorRef, ClaimRef, ExecutionId, StepRunId};
use tokio_postgres::Client;

async fn now_ms(c: &Client) -> Result<i64, StoreError> {
    c.query_one(
        "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
        &[],
    )
    .await
    .map(|row| row.get(0))
    .map_err(db)
}

/// Lock and fence the current claim row; same rules as the memory store.
async fn fence(
    store: &PostgresExecutionStore,
    c: &Client,
    id: &ExecutionId,
    step_run_id: &StepRunId,
    claim: &ClaimRef,
    owner: &ActorRef,
    now: i64,
) -> Result<ClaimWrite, StoreError> {
    if catalog::owner(c, id).await?.as_ref() != Some(&store.scope) {
        return Err(StoreError::NotFound);
    }
    let row = c
        .query_opt(
            "SELECT body FROM runbook.execution_claim \
             WHERE execution_id = $1 AND step_run_id = $2 FOR UPDATE",
            &[&id.as_str(), &step_run_id.as_str()],
        )
        .await
        .map_err(db)?
        .ok_or(StoreError::NotFound)?;
    let current: ClaimWrite = decode(row.get(0))?;
    if current.released
        || current.claim_id != claim.id
        || current.generation != claim.generation.get()
        || &current.owner != owner
        || current
            .lease_deadline_at_ms
            .is_some_and(|deadline| now >= deadline)
    {
        return Err(StoreError::InvalidBatch("queue.stale_lease"));
    }
    Ok(current)
}

impl ClaimStore for PostgresExecutionStore {
    async fn heartbeat(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        replay: Option<&runbook_core::actor::Replay>,
    ) -> Result<Option<i64>, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            self.lock_head(c, id).await?.ok_or(StoreError::NotFound)?;
            let now = now_ms(c).await?;
            let operation = format!("heartbeat:{}", step_run_id);
            if let Some(value) = replay_lookup(c, id, replay, &operation, now).await? {
                return Ok(value);
            }
            let mut current = fence(self, c, id, step_run_id, claim, owner, now).await?;
            if current.lease_deadline_at_ms.is_some() {
                current.lease_deadline_at_ms = Some(now + runbook_core::claims::AGENT_LEASE_MS);
            }
            c.execute(
                "UPDATE runbook.execution_claim SET body = $3, lease_deadline_at_ms = $4 \
                 WHERE execution_id = $1 AND step_run_id = $2",
                &[
                    &id.as_str(),
                    &step_run_id.as_str(),
                    &encode(&current)?,
                    &current.lease_deadline_at_ms,
                ],
            )
            .await
            .map_err(db)?;
            replay_write(c, id, replay, &operation, now, current.lease_deadline_at_ms).await?;
            self.checkpoint(runbook_core::WriteStage::Idempotency)?;
            Ok(current.lease_deadline_at_ms)
        })
        .await
    }

    async fn append_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        lines: Vec<String>,
        replay: Option<&runbook_core::actor::Replay>,
    ) -> Result<u64, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            self.lock_head(c, id).await?.ok_or(StoreError::NotFound)?;
            let now = now_ms(c).await?;
            let operation = format!("append_logs:{}", step_run_id);
            if let Some(value) = replay_lookup(c, id, replay, &operation, now).await? {
                return Ok(value);
            }
            fence(self, c, id, step_run_id, claim, owner, now).await?;
            // PRD §17: 5 MB of log text per step, fenced under the head lock;
            // each entry counts at least one byte (`log_entry_bytes`).
            let stored: i64 = c
                .query_one(
                    "SELECT COALESCE(sum(greatest(octet_length(line), 1)), 0)::bigint \
                     FROM runbook.claim_log WHERE execution_id = $1 AND step_run_id = $2",
                    &[&id.as_str(), &step_run_id.as_str()],
                )
                .await
                .map_err(db)?
                .get(0);
            let adding: u64 = lines
                .iter()
                .map(|line| runbook_core::claims::log_entry_bytes(line))
                .sum();
            if (stored as u64).saturating_add(adding) > runbook_core::claims::MAX_STEP_LOG_BYTES {
                return Err(StoreError::InvalidBatch("step.logs_too_large"));
            }
            let base: i64 = c
                .query_one(
                    "SELECT COALESCE(max(seq), 0) FROM runbook.claim_log \
                     WHERE execution_id = $1 AND step_run_id = $2",
                    &[&id.as_str(), &step_run_id.as_str()],
                )
                .await
                .map_err(db)?
                .get(0);
            for (index, line) in lines.iter().enumerate() {
                c.execute(
                    "INSERT INTO runbook.claim_log (execution_id, step_run_id, seq, line) \
                     VALUES ($1, $2, $3, $4)",
                    &[
                        &id.as_str(),
                        &step_run_id.as_str(),
                        &(base + 1 + index as i64),
                        line,
                    ],
                )
                .await
                .map_err(db)?;
            }
            let count = (base as u64) + lines.len() as u64;
            replay_write(c, id, replay, &operation, now, count).await?;
            self.checkpoint(runbook_core::WriteStage::Idempotency)?;
            Ok(count)
        })
        .await
    }

    async fn claim_record(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<Option<ClaimWrite>, StoreError> {
        let conn = self.pool.get().await.map_err(crate::rows::connection)?;
        if catalog::owner(conn.client(), id).await?.as_ref() != Some(&self.scope) {
            return Ok(None);
        }
        conn.client()
            .query_opt(
                "SELECT body FROM runbook.execution_claim \
                 WHERE execution_id = $1 AND step_run_id = $2",
                &[&id.as_str(), &step_run_id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|row| decode(row.get(0)))
            .transpose()
    }
    async fn read_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        let conn = self.pool.get().await.map_err(crate::rows::connection)?;
        if catalog::owner(conn.client(), id).await?.as_ref() != Some(&self.scope) {
            return Err(StoreError::NotFound);
        }
        let after = i64::try_from(after).unwrap_or(i64::MAX);
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        conn.client()
            .query(
                "SELECT seq, line FROM runbook.claim_log \
                 WHERE execution_id = $1 AND step_run_id = $2 AND seq > $3 \
                 ORDER BY seq LIMIT $4",
                &[&id.as_str(), &step_run_id.as_str(), &after, &limit],
            )
            .await
            .map_err(db)?
            .into_iter()
            .map(|row| Ok((row.get::<_, i64>(0) as u64, row.get(1))))
            .collect()
    }
}

impl PostgresExecutionStore {
    /// F46: the run and project of a step run whose run was purged by
    /// retention (migration 0024), so its log link answers 410, not 404.
    pub async fn expired_step_run(
        &self,
        org_id: &runbook_model::OrganizationId,
        step_run_id: &StepRunId,
    ) -> Result<Option<(ExecutionId, runbook_model::ProjectId)>, StoreError> {
        let conn = self.pool.get().await.map_err(crate::rows::connection)?;
        let invalid = |_| StoreError::Backend("postgres.invalid_stored_id");
        conn.client()
            .query_opt(
                "SELECT execution_id, project_id FROM runbook.step_run_tombstone \
                 WHERE org_id = $1 AND step_run_id = $2",
                &[&org_id.as_str(), &step_run_id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|row| {
                Ok((
                    row.get::<_, &str>(0).parse().map_err(invalid)?,
                    row.get::<_, &str>(1).parse().map_err(invalid)?,
                ))
            })
            .transpose()
    }
}

pub(crate) async fn replay_lookup<T: serde::de::DeserializeOwned>(
    c: &Client,
    id: &ExecutionId,
    replay: Option<&runbook_core::actor::Replay>,
    operation: &str,
    now: i64,
) -> Result<Option<T>, StoreError> {
    let Some(replay) = replay else {
        return Ok(None);
    };
    let row = crate::actor_replay::lookup(c, id, &replay.prefix(operation)?, now).await?;
    replay.lookup(operation, row.iter(), now)
}
pub(crate) async fn replay_write(
    c: &Client,
    id: &ExecutionId,
    replay: Option<&runbook_core::actor::Replay>,
    operation: &str,
    now: i64,
    value: impl serde::Serialize,
) -> Result<(), StoreError> {
    if let Some(replay) = replay {
        let row = replay.record(operation, id, now, value)?;
        crate::actor_replay::write(c, &row, now).await?;
    }
    Ok(())
}
