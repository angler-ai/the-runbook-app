//! CF5: what a run's object gives the hosts that reach it from outside
//! (the object protocol in `runbook-worker`): the run-owned rows a
//! retention export lists beside the run itself, purging the object after a
//! deletion committed in D1, and a session begun and committed in two
//! separate requests.
use crate::{
    DoSql, DoStore,
    sql::{Columns, decode},
};
use runbook_core::{
    CommitResult, ExecutionCommit, IdempotencyRecord, StoreError, StoredExecution,
    retention::WorkState,
    work_queue::{WorkId, WorkKind, WorkLease},
};
use runbook_model::ExecutionId;

/// A session's fence, for a session begun in one request and committed in
/// another: the snapshot, the store clock at `begin` and the `meta.writes`
/// value the commit checks. No gate is held between the two requests, so a
/// write in between turns the commit into a conflict (optimistic, where a
/// local session would have waited).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Fence {
    pub snapshot: StoredExecution,
    pub at_ms: i64,
    pub writes: u64,
}

/// What a fenced commit checks of its [`Fence`], without the snapshot (CF13
/// review round 1: a commit no longer ships the whole run back to the
/// object it came from).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FenceRef {
    pub execution_id: ExecutionId,
    pub revision: u64,
    pub completed_at_ms: Option<i64>,
    pub at_ms: i64,
    pub writes: u64,
}

impl Fence {
    /// What the commit of this session checks.
    pub fn to_ref(&self) -> FenceRef {
        FenceRef {
            execution_id: self.snapshot.projection.execution_id.clone(),
            revision: self.snapshot.revision,
            completed_at_ms: self.snapshot.projection.completed_at_ms,
            at_ms: self.at_ms,
            writes: self.writes,
        }
    }
}

impl<B: DoSql> DoStore<B> {
    /// The actor replay rows and work leases of `id` (the retention
    /// export's `actor_replays` and `work`), in key order.
    pub fn export_parts(
        &self,
        id: &ExecutionId,
    ) -> Result<(Vec<IdempotencyRecord>, Vec<WorkState>), StoreError> {
        self.owned_head(id)?;
        let replays = self
            .sql
            .query("SELECT body FROM actor_replay ORDER BY prefix, at_ms", &[])?
            .iter()
            .map(|row| decode(row.text(0)?))
            .collect::<Result<Vec<_>, _>>()?;
        let work = self
            .sql
            .query(
                "SELECT kind, work_key, work_generation, generation, deadline_at_ms, \
                 acknowledged FROM work_lease ORDER BY kind, work_key, work_generation",
                &[],
            )?
            .iter()
            .map(|row| {
                let key = row.text(1)?.to_owned();
                let generation = row.unsigned(2)?;
                let kind = match row.text(0)? {
                    "timer" => WorkKind::Timer { key, generation },
                    "notification" => WorkKind::Notification { event_id: key },
                    "activation" => WorkKind::Activation,
                    "claim" => WorkKind::ClaimLease {
                        step_run_id: key.parse().map_err(|_| StoreError::Backend("do.decode"))?,
                        generation,
                    },
                    _ => return Err(StoreError::Backend("do.decode")),
                };
                Ok(WorkState {
                    lease: WorkLease {
                        scope: self.scope.clone(),
                        id: WorkId {
                            execution_id: id.clone(),
                            kind,
                        },
                        generation: row.unsigned(3)?,
                        deadline_at_ms: row.integer(4)?,
                    },
                    acknowledged: row.integer(5)? != 0,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((replays, work))
    }

    /// Empty this object after its run's deletion committed in D1 (the
    /// tombstone there is the record): every row and the alarm, in one
    /// transaction, as `deleteAll` would, keeping the schema. True when it
    /// held `id`; false when it was already empty. Another tenant's or
    /// another run's object is `NotFound`.
    pub fn purge(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        match self.head()? {
            None => Ok(false),
            Some(_) => {
                self.owned_head(id)?;
                self.reset()?;
                Ok(true)
            }
        }
    }

    /// Begin a session to be committed by a later request
    /// ([`DoStore::commit_fenced`]).
    pub fn begin_fenced(&self, id: &ExecutionId) -> Result<Fence, StoreError> {
        let head = self.owned_head(id)?;
        Ok(Fence {
            snapshot: self.load_in(id)?,
            at_ms: self.sql.now_ms(),
            writes: head.writes,
        })
    }

    /// Commit a session begun with [`DoStore::begin_fenced`], with a local
    /// session's checks: the batch's run and expected revision, the
    /// transaction time, and the fence.
    pub fn commit_fenced(
        &self,
        batch: ExecutionCommit,
        fence: &Fence,
    ) -> Result<CommitResult, StoreError> {
        self.commit_fenced_ref(batch, &fence.to_ref())
    }

    /// [`DoStore::commit_fenced`] with only what the fence checks.
    pub fn commit_fenced_ref(
        &self,
        batch: ExecutionCommit,
        fence: &FenceRef,
    ) -> Result<CommitResult, StoreError> {
        if batch.execution_id != fence.execution_id {
            return Err(StoreError::NotFound);
        }
        if batch.expected_sequence != fence.revision {
            return Ok(CommitResult::Conflict);
        }
        batch.check_transaction_time(fence.at_ms, fence.completed_at_ms)?;
        self.commit_at(batch, Some(fence.at_ms), Some(fence.writes))
    }

    /// The newest actor replay under `prefix` at a session's time.
    pub fn replay_at(
        &self,
        id: &ExecutionId,
        prefix: &str,
        at_ms: i64,
    ) -> Result<Option<IdempotencyRecord>, StoreError> {
        self.owned_head(id)?;
        self.lookup_replay(prefix, at_ms)
    }
}
