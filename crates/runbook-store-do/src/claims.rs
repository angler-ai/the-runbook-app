//! `ClaimStore` (B6a): heartbeats and appended logs that carry no engine
//! event, fenced by claim generation and owner, with actor replay. Claims
//! themselves change inside execution commits (`commit.rs`).
use crate::{
    DoSql, DoStore,
    sql::{Columns, decode, int, uint},
};
use runbook_core::{ClaimWrite, StoreError, WriteStage, actor::Replay, claims::ClaimStore};
use runbook_model::{ActorRef, ClaimRef, ExecutionId, StepRunId};

impl<B: DoSql> DoStore<B> {
    fn claim_row(&self, step_run_id: &StepRunId) -> Result<Option<ClaimWrite>, StoreError> {
        self.sql
            .query(
                "SELECT body FROM claim WHERE step_run_id = ?",
                &[step_run_id.as_str().into()],
            )?
            .first()
            .map(|row| decode(row.text(0)?))
            .transpose()
    }

    /// The current claim, only if it is this ref's live, unexpired lease.
    fn claim_fence(
        &self,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        now: i64,
    ) -> Result<ClaimWrite, StoreError> {
        let current = self.claim_row(step_run_id)?.ok_or(StoreError::NotFound)?;
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

    /// A saved reply for this replay key, if the actor already did this.
    fn replayed<T: serde::de::DeserializeOwned>(
        &self,
        replay: Option<&Replay>,
        operation: &str,
        now: i64,
    ) -> Result<Option<T>, StoreError> {
        let Some(replay) = replay else {
            return Ok(None);
        };
        let saved = self.lookup_replay(&replay.prefix(operation)?, now)?;
        replay.lookup(operation, saved.iter(), now)
    }
    fn record_replay<T: serde::Serialize>(
        &self,
        id: &ExecutionId,
        replay: Option<&Replay>,
        operation: &str,
        now: i64,
        value: T,
    ) -> Result<(), StoreError> {
        if let Some(replay) = replay {
            let row = replay.record(operation, id, now, value)?;
            self.write_replay(id, &row, now)?;
            self.checkpoint(WriteStage::Idempotency)?;
        }
        Ok(())
    }
}

impl<B: DoSql> ClaimStore for DoStore<B> {
    async fn heartbeat(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        replay: Option<&Replay>,
    ) -> Result<Option<i64>, StoreError> {
        let (id, step_run_id, claim, owner) = (
            id.clone(),
            step_run_id.clone(),
            claim.clone(),
            owner.clone(),
        );
        let replay = replay.cloned();
        self.write(move |store| {
            store.owned_head(&id)?;
            let now = store.sql.now_ms();
            let operation = format!("heartbeat:{step_run_id}");
            if let Some(saved) = store.replayed(replay.as_ref(), &operation, now)? {
                return Ok(saved);
            }
            let mut current = store.claim_fence(&step_run_id, &claim, &owner, now)?;
            if current.lease_deadline_at_ms.is_some() {
                current.lease_deadline_at_ms = Some(now + runbook_core::claims::AGENT_LEASE_MS);
            }
            let deadline = current.lease_deadline_at_ms;
            store.write_claim(&id, &current)?;
            store.bump_writes()?;
            store.record_replay(&id, replay.as_ref(), &operation, now, deadline)?;
            Ok(deadline)
        })
    }

    async fn append_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        claim: &ClaimRef,
        owner: &ActorRef,
        lines: Vec<String>,
        replay: Option<&Replay>,
    ) -> Result<u64, StoreError> {
        let (id, step_run_id, claim, owner) = (
            id.clone(),
            step_run_id.clone(),
            claim.clone(),
            owner.clone(),
        );
        let replay = replay.cloned();
        self.write(move |store| {
            store.owned_head(&id)?;
            let now = store.sql.now_ms();
            let operation = format!("append_logs:{step_run_id}");
            if let Some(saved) = store.replayed(replay.as_ref(), &operation, now)? {
                return Ok(saved);
            }
            store.claim_fence(&step_run_id, &claim, &owner, now)?;
            // CF5: lines already moved to R2 chunks count too (`log_chunk`,
            // whose `bytes` uses the same measure). Each entry counts at
            // least one byte (`log_entry_bytes`).
            let totals = store.sql.query(
                "SELECT max(COALESCE((SELECT max(seq) FROM claim_log WHERE step_run_id = ?1), 0), \
                   COALESCE((SELECT max(last_seq) FROM log_chunk WHERE step_run_id = ?1), 0)), \
                 COALESCE((SELECT SUM(max(length(CAST(line AS BLOB)), 1)) FROM claim_log \
                   WHERE step_run_id = ?1), 0) \
                 + COALESCE((SELECT SUM(bytes) FROM log_chunk WHERE step_run_id = ?1), 0)",
                &[step_run_id.as_str().into()],
            )?;
            let totals = totals.first().ok_or(StoreError::Backend("do.decode"))?;
            let (count, bytes) = (totals.unsigned(0)?, totals.unsigned(1)?);
            let added: u64 = lines
                .iter()
                .map(|line| runbook_core::claims::log_entry_bytes(line))
                .sum();
            if bytes.saturating_add(added) > runbook_core::claims::MAX_STEP_LOG_BYTES {
                return Err(StoreError::InvalidBatch("step.logs_too_large"));
            }
            let mut seq = count;
            for line in &lines {
                seq += 1;
                store.sql.exec(
                    "INSERT INTO claim_log (org_id, project_id, execution_id, step_run_id, seq, \
                     line) VALUES (?, ?, ?, ?, ?, ?)",
                    &store.owned(
                        &id,
                        vec![
                            step_run_id.as_str().into(),
                            uint(seq)?,
                            line.as_str().into(),
                        ],
                    ),
                )?;
            }
            store.record_replay(&id, replay.as_ref(), &operation, now, seq)?;
            Ok(seq)
        })
    }

    async fn claim_record(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<Option<ClaimWrite>, StoreError> {
        match self.owned_head(id) {
            Ok(_) => self.claim_row(step_run_id),
            Err(StoreError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn read_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        self.owned_head(id)?;
        self.sql
            .query(
                "SELECT seq, line FROM claim_log WHERE step_run_id = ? AND seq > ? \
                 ORDER BY seq LIMIT ?",
                &[
                    step_run_id.as_str().into(),
                    uint(after)?,
                    int(i64::try_from(limit).unwrap_or(i64::MAX).min(100_000))?,
                ],
            )?
            .iter()
            .map(|row| Ok((row.unsigned(0)?, row.text(1)?.to_owned())))
            .collect()
    }
}
