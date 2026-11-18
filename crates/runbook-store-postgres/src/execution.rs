//! A22 ExecutionStore on PostgreSQL. One READ COMMITTED transaction locks the
//! execution head, checks the expected sequence, and writes every row or none.
use crate::{
    DatabaseConfig, catalog,
    pool::{Conn, Pool},
    rows::{connection, db, decode, encode, int, is_unique_violation, label, uint},
};
use runbook_core::{
    CommitResult, ExecutionCommit, ExecutionProjection, ExecutionStore, StoreError,
    StoredExecution, TimerChange, TimerRecord, WriteStage,
    catalog::TenantScope,
    session::{ExecutionSession, SessionStore},
};
use runbook_model::ExecutionId;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use tokio_postgres::Client;

pub(crate) const READ_COMMITTED: &str = "BEGIN ISOLATION LEVEL READ COMMITTED";
pub(crate) const SNAPSHOT_READ: &str = "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY";

/// Tenant-bound handle. Clones share one connection pool; futures are not Send.
#[derive(Clone)]
pub struct PostgresExecutionStore {
    pub(crate) pool: Rc<Pool>,
    pub(crate) scope: TenantScope,
    fail: Rc<Cell<Option<WriteStage>>>,
    pause: Rc<RefCell<OwnerCheckPause>>,
    /// ARC test seam: runs once before the next archive-file put.
    pub(crate) archive_put_hook: Rc<RefCell<Option<ArchivePutHook>>>,
    /// ARC test seam: how many `record_archive` attempts fail as if the
    /// database reported a deadlock (40P01) before one runs.
    pub(crate) retry_faults: Rc<Cell<u32>>,
    /// ARC test seam: how many `record_archive` retries this handle took.
    pub(crate) retries_taken: Rc<Cell<u32>>,
    /// ARC test seam: pause `record_archive` after it locks the file's row.
    pub(crate) record_pause: Rc<RefCell<OwnerCheckPause>>,
    /// STO1 test seam: how many blob `put` transactions fail before one runs.
    pub(crate) blob_put_faults: Rc<Cell<u32>>,
    /// CF6 quota limits, from the config (off by default).
    pub(crate) quota: runbook_core::quotas::QuotaLimits,
    /// CF13 review round 1: the run budget, from the config.
    pub(crate) run_budget: u64,
    /// CF6 test seam: how far ahead of the database clock quota months run
    /// (Postgres has no test clock).
    pub(crate) quota_clock_offset_ms: i64,
    /// STO1: optional object storage for blobs (GCS, S3, InMemory).
    pub(crate) objects: Option<std::sync::Arc<runbook_store_objects::ObjectStorage>>,
}

/// An async test hook (`before_next_archive_put`).
pub(crate) type ArchivePutHook =
    Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>>;

#[derive(Default)]
pub(crate) struct OwnerCheckPause {
    armed: bool,
    paused: bool,
    released: bool,
}

pub(crate) struct Head {
    pub(crate) revision: u64,
    pub(crate) projection: ExecutionProjection,
}

impl PostgresExecutionStore {
    pub fn new(config: DatabaseConfig, scope: TenantScope) -> Self {
        Self {
            quota: config.quota_limits(),
            run_budget: config.run_budget(),
            quota_clock_offset_ms: 0,
            pool: Pool::new(config),
            scope,
            fail: Rc::default(),
            pause: Rc::default(),
            archive_put_hook: Rc::default(),
            retry_faults: Rc::default(),
            retries_taken: Rc::default(),
            record_pause: Rc::default(),
            blob_put_faults: Rc::default(),
            objects: None,
        }
    }
    /// STO1: the same pool with object storage configured.
    pub fn with_objects(
        &self,
        objects: Option<std::sync::Arc<runbook_store_objects::ObjectStorage>>,
    ) -> Self {
        Self {
            objects,
            ..self.clone()
        }
    }
    /// STO1: the object storage handle, if configured.
    pub fn objects(&self) -> Option<&std::sync::Arc<runbook_store_objects::ObjectStorage>> {
        self.objects.as_ref()
    }
    /// STO1: sweep orphan blobs from object storage.
    pub async fn sweep_blobs(&self, min_age: std::time::Duration) -> Result<usize, StoreError> {
        let Some(objects) = &self.objects else {
            return Ok(0);
        };
        let conn = self.pool.get().await.map_err(connection)?;
        let locked = runbook_store_objects::sweeper::try_acquire_sweeper_lock(conn.client())
            .await
            .map_err(db)?;
        if !locked {
            return Ok(0);
        }
        let result =
            runbook_store_objects::sweeper::sweep_orphans(conn.client(), objects, min_age).await;
        let _ = runbook_store_objects::sweeper::release_sweeper_lock(conn.client()).await;
        result.map_err(|_| StoreError::Backend("blob.sweep_failed"))
    }
    /// CF6: the same pool with `limits` in force.
    pub fn with_quota(&self, limits: runbook_core::quotas::QuotaLimits) -> Self {
        Self {
            quota: limits,
            ..self.clone()
        }
    }
    /// Test hook (CF6): quota months run `ms` ahead of the database clock.
    #[doc(hidden)]
    pub fn with_quota_clock_offset(&self, ms: i64) -> Self {
        Self {
            quota_clock_offset_ms: ms,
            ..self.clone()
        }
    }
    /// Test hook: run `hook` once, just before the next archive-file put
    /// (outside any transaction), so a test can interleave a deletion or a
    /// second writer inside a rewrite.
    #[doc(hidden)]
    pub fn before_next_archive_put(
        &self,
        hook: impl FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> + 'static,
    ) {
        *self.archive_put_hook.borrow_mut() = Some(Box::new(hook));
    }
    /// Test hook: the next `n` `record_archive` transactions fail with
    /// `postgres.retry_transaction`, as a deadlock (40P01) or serialization
    /// failure (40001) maps.
    #[doc(hidden)]
    pub fn inject_record_retries(&self, n: u32) {
        self.retry_faults.set(n);
    }
    /// Test hook: the next `n` blob `put` transactions fail with
    /// `postgres.injected_fault`.
    #[doc(hidden)]
    pub fn inject_blob_put_faults(&self, n: u32) {
        self.blob_put_faults.set(n);
    }
    /// Test hook: pause the next `record_archive` transaction right after it
    /// locks the file's row (its run checks are done), until released.
    #[doc(hidden)]
    pub fn pause_record_after_lock(&self) {
        self.record_pause.borrow_mut().armed = true;
    }
    #[doc(hidden)]
    pub fn record_paused(&self) -> bool {
        self.record_pause.borrow().paused
    }
    #[doc(hidden)]
    pub fn release_record(&self) {
        self.record_pause.borrow_mut().released = true;
    }
    pub(crate) async fn record_pause_point(&self) {
        if !self.record_pause.borrow().armed {
            return;
        }
        self.record_pause.borrow_mut().paused = true;
        while !self.record_pause.borrow().released {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        *self.record_pause.borrow_mut() = OwnerCheckPause::default();
    }
    /// Test hook: how many `record_archive` transactions were retried.
    #[doc(hidden)]
    pub fn record_retries_taken(&self) -> u32 {
        self.retries_taken.get()
    }
    /// Same pool, different tenant. Every query still filters by this scope.
    pub fn scoped(&self, scope: TenantScope) -> Self {
        Self {
            scope,
            ..self.clone()
        }
    }
    /// Test hook: fail the next commit after the named write stage.
    #[doc(hidden)]
    pub fn fail_after(&self, stage: WriteStage) {
        self.fail.set(Some(stage));
    }
    /// Test hook: pause inside lock_head after the owner check, so a test can
    /// interleave another tenant's create before the head lock (R6).
    #[doc(hidden)]
    pub fn pause_after_owner_check(&self) {
        self.pause.borrow_mut().armed = true;
    }
    #[doc(hidden)]
    pub fn owner_check_paused(&self) -> bool {
        self.pause.borrow().paused
    }
    #[doc(hidden)]
    pub fn release_owner_check(&self) {
        self.pause.borrow_mut().released = true;
    }
    async fn owner_check_pause(&self) {
        if !self.pause.borrow().armed {
            return;
        }
        self.pause.borrow_mut().paused = true;
        while !self.pause.borrow().released {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        *self.pause.borrow_mut() = OwnerCheckPause::default();
    }
    pub(crate) fn checkpoint(&self, stage: WriteStage) -> Result<(), StoreError> {
        if self.fail.get() == Some(stage) {
            self.fail.set(None);
            return Err(StoreError::Backend("injected"));
        }
        Ok(())
    }

    /// Database wall clock, independent of any transaction start time.
    pub async fn backend_time_ms(&self) -> Result<i64, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_one(
                "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                &[],
            )
            .await
            .map(|row| row.get(0))
            .map_err(db)
    }

    /// Create an execution head at revision 0 for this tenant.
    pub async fn create_execution(
        &self,
        projection: ExecutionProjection,
    ) -> Result<(), StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            if crate::retention::tombstoned(c, &projection.execution_id).await? {
                return Err(StoreError::NotFound);
            }
            catalog::claim_owner(c, &self.scope, &projection.execution_id).await?;
            if !insert_head(c, &projection).await? {
                return Err(StoreError::InvalidBatch("store.execution_exists"));
            }
            Ok(())
        })
        .await
    }

    /// Retry one recorded derived update; never a caller-supplied row.
    pub async fn redeliver_derived(
        &self,
        id: &ExecutionId,
        sequence: u64,
    ) -> Result<(), StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            if catalog::owner(c, id).await?.as_ref() != Some(&self.scope) {
                return Err(StoreError::NotFound);
            }
            let row = c
                .query_opt(
                    "SELECT body FROM runbook.execution_derived_outbox \
                     WHERE execution_id = $1 AND sequence = $2",
                    &[&id.as_str(), &int(sequence)?],
                )
                .await
                .map_err(db)?
                .ok_or(StoreError::NotFound)?;
            catalog::apply_derived(c, &self.scope, decode(row.get(0))?).await
        })
        .await
    }

    /// Check out one connection and begin at the given isolation statement.
    /// The caller owns finishing it; sessions hold theirs past this call.
    async fn open(&self, isolation: &str) -> Result<Conn, StoreError> {
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(isolation).await.map_err(db)?;
        Ok(conn)
    }

    pub(crate) async fn transaction<T>(
        &self,
        isolation: &str,
        body: impl AsyncFnOnce(&Client) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut conn = self.open(isolation).await?;
        let result = body(conn.client()).await;
        finish(&mut conn, result).await
    }

    /// Owner check, then lock the head row with tenant predicates in the same
    /// statement. A foreign execution is NotFound, whichever statement sees
    /// it; an owner with no head yet (admission claimed it) reads as no head.
    pub(crate) async fn lock_head(
        &self,
        c: &Client,
        id: &ExecutionId,
    ) -> Result<Option<Head>, StoreError> {
        if catalog::owner(c, id)
            .await?
            .is_some_and(|owner| owner != self.scope)
        {
            return Err(StoreError::NotFound);
        }
        self.owner_check_pause().await;
        let head = c
            .query_opt(
                "SELECT e.revision, e.projection FROM runbook.execution e \
                 JOIN runbook.execution_owner o USING (execution_id) \
                 WHERE e.execution_id = $1 AND o.org_id = $2 AND o.project_id = $3 \
                 FOR UPDATE OF e",
                &[
                    &id.as_str(),
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                ],
            )
            .await
            .map_err(db)?
            .map(|row| {
                Ok(Head {
                    revision: uint(row.get(0))?,
                    projection: decode(row.get(1))?,
                })
            })
            .transpose()?;
        if head.is_some() {
            return Ok(head);
        }
        // No owned head. A foreign owner created since the first check is
        // still NotFound; no owner (or our own claim) means no head yet.
        if catalog::owner(c, id)
            .await?
            .is_some_and(|owner| owner != self.scope)
        {
            return Err(StoreError::NotFound);
        }
        Ok(None)
    }

    /// Checks then writes, inside the caller's transaction with the head locked.
    pub(crate) async fn apply_commit(
        &self,
        c: &Client,
        batch: ExecutionCommit,
        head: Option<Head>,
        session_time: Option<i64>,
    ) -> Result<CommitResult, StoreError> {
        let id = batch.execution_id.as_str();
        if head.as_ref().map_or(0, |h| h.revision) != batch.expected_sequence {
            return Ok(CommitResult::Conflict);
        }
        let sequence = batch.validate()?;
        if batch.events.is_empty() && head.is_none() {
            return Err(StoreError::InvalidBatch("store.replay_projection_changed"));
        }
        if let Some(head) = &head {
            let (old, new) = (&head.projection, &batch.projection);
            if batch.events.is_empty() && old != new {
                return Err(StoreError::InvalidBatch("store.replay_projection_changed"));
            }
            if old.runbook_id != new.runbook_id
                || old.version_id != new.version_id
                || old.compiled_hash != new.compiled_hash
            {
                return Err(StoreError::InvalidBatch("store.pinned_version_changed"));
            }
            let ids: Vec<&str> = batch.outbox.iter().map(|i| i.event_id.as_str()).collect();
            if exists(c, "execution_outbox", "event_id", id, &ids).await? {
                return Err(StoreError::InvalidBatch("store.duplicate_outbox"));
            }
        }
        for change in &batch.timers {
            let TimerChange::Upsert(timer) = change else {
                continue;
            };
            let active: Option<TimerRecord> =
                timer_row(c, "execution_timer", id, &timer.key).await?;
            let prior = match timer_row(c, "execution_timer_history", id, &timer.key).await? {
                Some(history) => Some(history),
                None => active.clone(),
            };
            if let Some(prior) = prior
                && (timer.generation < prior.generation
                    || (timer.generation == prior.generation && active.as_ref() != Some(timer)))
            {
                return Err(StoreError::InvalidBatch("store.timer_generation_reused"));
            }
        }
        for claim in &batch.claims {
            let generation = c
                .query_opt(
                    "SELECT generation FROM runbook.execution_claim \
                     WHERE execution_id = $1 AND step_run_id = $2",
                    &[&id, &claim.step_run_id.as_str()],
                )
                .await
                .map_err(db)?
                .map_or(Ok(0), |row| uint(row.get(0)))?;
            if generation != claim.expected_generation {
                return Ok(CommitResult::Conflict);
            }
        }
        for write in &batch.credentials {
            crate::execution_credentials::precheck(c, &self.scope, &batch.execution_id, write)
                .await?;
        }
        let keys: Vec<&str> = batch
            .idempotency
            .iter()
            .filter(|r| !r.key.starts_with("actor:"))
            .map(|r| r.key.as_str())
            .collect();
        if exists(c, "execution_idempotency", "key", id, &keys).await? {
            return Err(StoreError::InvalidBatch("store.idempotency_conflict"));
        }

        // Another tenant won a creation race: answer exactly as for any foreign ID.
        match catalog::claim_owner(c, &self.scope, &batch.execution_id).await {
            Err(StoreError::InvalidBatch("catalog.execution_scope_conflict")) => {
                return Err(StoreError::NotFound);
            }
            other => other?,
        }
        // A lost creation race is an ordinary conflict; the caller reloads and retries.
        if head.is_none() && !insert_head(c, &batch.projection).await? {
            return Ok(CommitResult::Conflict);
        }
        let seqs = batch
            .events
            .iter()
            .map(|e| int(e.seq))
            .collect::<Result<Vec<_>, _>>()?;
        let bodies = batch
            .events
            .iter()
            .map(encode)
            .collect::<Result<Vec<_>, _>>()?;
        c.execute(
            "INSERT INTO runbook.execution_event (execution_id, seq, body) \
             SELECT $1, * FROM unnest($2::bigint[], $3::text[])",
            &[&id, &seqs, &bodies],
        )
        .await
        .map_err(db)?;
        // ARC1: new events on a run already in a daily file (an amendment)
        // make that file stale in this same transaction.
        if !batch.events.is_empty() {
            crate::archives::mark_run_stale(c, id).await?;
        }
        self.checkpoint(WriteStage::Events)?;
        // Hint only, delivered by PostgreSQL at commit; a rollback anywhere
        // later in this transaction delivers nothing (B2.3).
        c.execute(
            "SELECT pg_notify($1, $2)",
            &[
                &crate::live_events::CHANNEL,
                &crate::live_events::payload(&batch.execution_id, sequence),
            ],
        )
        .await
        .map_err(db)?;

        let p = &batch.projection;
        c.execute(
            "UPDATE runbook.execution SET runbook_id = $2, status = $3, \
             completed_at_ms = $4, projection = $5 WHERE execution_id = $1",
            &[
                &id,
                &p.runbook_id.as_str(),
                &label(&p.status)?,
                &p.completed_at_ms,
                &encode(p)?,
            ],
        )
        .await
        .map_err(db)?;
        let previous_steps: std::collections::BTreeSet<_> = head
            .as_ref()
            .map(|h| h.projection.steps.iter().map(|s| &s.step_run_id).collect())
            .unwrap_or_default();
        for step in &p.steps {
            if !previous_steps.contains(&step.step_run_id) {
                crate::step_lookup::insert(c, &self.scope, &batch.execution_id, &step.step_run_id)
                    .await?;
            }
        }
        self.checkpoint(WriteStage::Projection)?;

        for change in &batch.timers {
            match change {
                TimerChange::Upsert(timer) => {
                    let body = encode(timer)?;
                    for table in ["execution_timer_history", "execution_timer"] {
                        c.execute(
                            &format!(
                                "INSERT INTO runbook.{table} (execution_id, key, body) \
                                 VALUES ($1, $2, $3) ON CONFLICT (execution_id, key) \
                                 DO UPDATE SET body = EXCLUDED.body"
                            ),
                            &[&id, &timer.key, &body],
                        )
                        .await
                        .map_err(db)?;
                    }
                }
                TimerChange::Delete { key, .. } => {
                    c.execute(
                        "INSERT INTO runbook.execution_timer_history (execution_id, key, body) \
                         SELECT execution_id, key, body FROM runbook.execution_timer \
                         WHERE execution_id = $1 AND key = $2 \
                         ON CONFLICT (execution_id, key) DO UPDATE SET body = EXCLUDED.body",
                        &[&id, key],
                    )
                    .await
                    .map_err(db)?;
                    c.execute(
                        "DELETE FROM runbook.execution_timer WHERE execution_id = $1 AND key = $2",
                        &[&id, key],
                    )
                    .await
                    .map_err(db)?;
                }
            }
        }
        self.checkpoint(WriteStage::Timers)?;

        for intent in &batch.outbox {
            c.execute(
                "INSERT INTO runbook.execution_outbox (execution_id, event_id, body) \
                 VALUES ($1, $2, $3)",
                &[&id, &intent.event_id, &encode(intent)?],
            )
            .await
            .map_err(db)?;
        }
        self.checkpoint(WriteStage::Outbox)?;

        let replay_time = if batch
            .idempotency
            .iter()
            .any(|r| r.key.starts_with("actor:"))
        {
            Some(match session_time {
                Some(at) => at,
                None => c
                    .query_one(
                        "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                        &[],
                    )
                    .await
                    .map_err(db)?
                    .get(0),
            })
        } else {
            None
        };
        for record in &batch.idempotency {
            if runbook_core::actor::replay_record_parts(record)?.is_some() {
                crate::actor_replay::write(
                    c,
                    record,
                    replay_time.ok_or(StoreError::InvalidBatch("replay.missing_time"))?,
                )
                .await?;
                continue;
            }
            c.execute(
                "INSERT INTO runbook.execution_idempotency (execution_id, key, body) \
                 VALUES ($1, $2, $3)",
                &[&id, &record.key, &encode(record)?],
            )
            .await
            .map_err(db)?;
        }
        self.checkpoint(WriteStage::Idempotency)?;

        for claim in &batch.claims {
            c.execute(
                "INSERT INTO runbook.execution_claim \
                 (execution_id, step_run_id, generation, body, lease_deadline_at_ms, released) \
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (execution_id, step_run_id) \
                 DO UPDATE SET generation = EXCLUDED.generation, body = EXCLUDED.body, \
                 lease_deadline_at_ms = EXCLUDED.lease_deadline_at_ms, \
                 released = EXCLUDED.released",
                &[
                    &id,
                    &claim.step_run_id.as_str(),
                    &int(claim.generation)?,
                    &encode(claim)?,
                    &claim.lease_deadline_at_ms,
                    &claim.released,
                ],
            )
            .await
            .map_err(db)?;
        }
        self.checkpoint(WriteStage::Claims)?;
        for write in batch.credentials.clone() {
            crate::execution_credentials::apply(c, write).await?;
        }
        self.checkpoint(WriteStage::Credentials)?;
        if matches!(
            batch.projection.status,
            runbook_model::ExecutionStatus::Succeeded
                | runbook_model::ExecutionStatus::Failed
                | runbook_model::ExecutionStatus::Cancelled
        ) {
            let closed_at = batch.events.last().map_or(0, |event| event.at_ms);
            crate::execution_credentials::revoke_all(c, &batch.execution_id, closed_at).await?;
        }

        for update in &batch.derived_outbox {
            catalog::apply_derived(c, &self.scope, update.clone()).await?;
            c.execute(
                "INSERT INTO runbook.execution_derived_outbox (execution_id, sequence, body) \
                 VALUES ($1, $2, $3)",
                &[&id, &int(update.summary.sequence)?, &encode(update)?],
            )
            .await
            .map_err(db)?;
        }
        self.checkpoint(WriteStage::DerivedRows)?;

        c.execute(
            "UPDATE runbook.execution SET revision = $2 WHERE execution_id = $1",
            &[&id, &int(sequence)?],
        )
        .await
        .map_err(db)?;
        Ok(CommitResult::Committed(sequence))
    }
}

/// Commit a write; a conflict or error rolls back so no partial work survives.
async fn finish_commit(
    conn: &mut Conn,
    result: Result<CommitResult, StoreError>,
) -> Result<CommitResult, StoreError> {
    match result {
        Ok(CommitResult::Conflict) => {
            conn.rollback().await;
            Ok(CommitResult::Conflict)
        }
        other => finish(conn, other).await,
    }
}

/// Commit on success; roll back and keep the original error otherwise.
pub(crate) async fn finish<T>(
    conn: &mut Conn,
    result: Result<T, StoreError>,
) -> Result<T, StoreError> {
    match result {
        Ok(value) => {
            conn.commit().await.map_err(db)?;
            Ok(value)
        }
        Err(error) => {
            conn.rollback().await;
            Err(error)
        }
    }
}

/// False when another transaction already created this head.
pub(crate) async fn insert_head(c: &Client, p: &ExecutionProjection) -> Result<bool, StoreError> {
    let inserted = c
        .execute(
            "INSERT INTO runbook.execution \
         (execution_id, revision, runbook_id, status, completed_at_ms, projection) \
         VALUES ($1, 0, $2, $3, $4, $5)",
            &[
                &p.execution_id.as_str(),
                &p.runbook_id.as_str(),
                &label(&p.status)?,
                &p.completed_at_ms,
                &encode(p)?,
            ],
        )
        .await;
    match inserted {
        Ok(_) => Ok(true),
        Err(error) if is_unique_violation(&error) => Ok(false),
        Err(error) => Err(db(error)),
    }
}

async fn exists(
    c: &Client,
    table: &str,
    column: &str,
    id: &str,
    keys: &[&str],
) -> Result<bool, StoreError> {
    if keys.is_empty() {
        return Ok(false);
    }
    c.query_one(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM runbook.{table} \
             WHERE execution_id = $1 AND {column} = ANY($2))"
        ),
        &[&id, &keys],
    )
    .await
    .map(|row| row.get(0))
    .map_err(db)
}

async fn timer_row(
    c: &Client,
    table: &str,
    id: &str,
    key: &str,
) -> Result<Option<TimerRecord>, StoreError> {
    c.query_opt(
        &format!("SELECT body FROM runbook.{table} WHERE execution_id = $1 AND key = $2"),
        &[&id, &key],
    )
    .await
    .map_err(db)?
    .map(|row| decode(row.get(0)))
    .transpose()
}

/// Every execution-owned row, read inside the caller's transaction.
pub(crate) async fn read_stored(c: &Client, head: Head) -> Result<StoredExecution, StoreError> {
    let id = head.projection.execution_id.as_str();
    let bodies = async |sql: &str| -> Result<Vec<String>, StoreError> {
        Ok(c.query(sql, &[&id])
            .await
            .map_err(db)?
            .iter()
            .map(|row| row.get(0))
            .collect())
    };
    let mut run = StoredExecution {
        revision: head.revision,
        projection: head.projection.clone(),
        events: vec![],
        timers: Default::default(),
        timer_history: Default::default(),
        outbox: Default::default(),
        idempotency: Default::default(),
        claims: Default::default(),
        derived_outbox: Default::default(),
    };
    for body in
        bodies("SELECT body FROM runbook.execution_event WHERE execution_id = $1 ORDER BY seq")
            .await?
    {
        run.events.push(decode(&body)?);
    }
    for body in bodies("SELECT body FROM runbook.execution_timer WHERE execution_id = $1").await? {
        let timer: TimerRecord = decode(&body)?;
        run.timers.insert(timer.key.clone(), timer);
    }
    for body in
        bodies("SELECT body FROM runbook.execution_timer_history WHERE execution_id = $1").await?
    {
        let timer: TimerRecord = decode(&body)?;
        run.timer_history.insert(timer.key.clone(), timer);
    }
    for body in bodies("SELECT body FROM runbook.execution_outbox WHERE execution_id = $1").await? {
        let intent: runbook_engine::NotificationIntent = decode(&body)?;
        run.outbox.insert(intent.event_id.clone(), intent);
    }
    for body in
        bodies("SELECT body FROM runbook.execution_idempotency WHERE execution_id = $1").await?
    {
        let record: runbook_core::IdempotencyRecord = decode(&body)?;
        run.idempotency.insert(record.key.clone(), record);
    }
    for body in bodies("SELECT body FROM runbook.execution_claim WHERE execution_id = $1").await? {
        let claim: runbook_core::ClaimWrite = decode(&body)?;
        run.claims.insert(claim.step_run_id.clone(), claim);
    }
    for body in
        bodies("SELECT body FROM runbook.execution_derived_outbox WHERE execution_id = $1").await?
    {
        let update: runbook_core::catalog::DerivedUpdate = decode(&body)?;
        run.derived_outbox.insert(update.summary.sequence, update);
    }
    Ok(run)
}

impl ExecutionStore for PostgresExecutionStore {
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            let row = c
                .query_opt(
                    "SELECT e.revision, e.projection FROM runbook.execution e \
                     JOIN runbook.execution_owner o USING (execution_id) \
                     WHERE e.execution_id = $1 AND o.org_id = $2 AND o.project_id = $3",
                    &[
                        &id.as_str(),
                        &self.scope.org_id.as_str(),
                        &self.scope.project_id.as_str(),
                    ],
                )
                .await
                .map_err(db)?
                .ok_or(StoreError::NotFound)?;
            let head = Head {
                revision: uint(row.get(0))?,
                projection: decode(row.get(1))?,
            };
            read_stored(c, head).await
        })
        .await
    }

    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        let mut conn = self.open(READ_COMMITTED).await?;
        let result = async {
            let c = conn.client();
            if crate::retention::tombstoned(c, &batch.execution_id).await? {
                return Err(StoreError::NotFound);
            }
            let head = self.lock_head(c, &batch.execution_id).await?;
            self.apply_commit(c, batch, head, None).await
        }
        .await;
        finish_commit(&mut conn, result).await
    }
}

/// Holds one open transaction and the execution row lock from begin to commit.
/// Dropping it without committing discards the connection; the server rolls back.
pub struct PostgresSession {
    store: PostgresExecutionStore,
    conn: RefCell<Option<Conn>>,
    snapshot: StoredExecution,
    at_ms: i64,
    consumed: Cell<bool>,
}
impl ExecutionSession for PostgresSession {
    fn transaction_time_ms(&self) -> i64 {
        self.at_ms
    }
    async fn release(&self) {
        self.consumed.set(true);
        let conn = self.conn.borrow_mut().take();
        if let Some(mut conn) = conn {
            conn.rollback().await;
        }
    }
    async fn actor_replay(
        &self,
        prefix: &str,
    ) -> Result<Option<runbook_core::IdempotencyRecord>, StoreError> {
        if self.consumed.get() {
            return Err(StoreError::InvalidBatch("store.session_closed"));
        }
        let conn = self
            .conn
            .borrow_mut()
            .take()
            .ok_or(StoreError::InvalidBatch("store.session_closed"))?;
        let result = crate::actor_replay::lookup(
            conn.client(),
            &self.snapshot.projection.execution_id,
            prefix,
            self.at_ms,
        )
        .await;
        self.conn.replace(Some(conn));
        result
    }
}
impl SessionStore for PostgresExecutionStore {
    type Session = PostgresSession;
    fn run_budget(&self) -> u64 {
        self.run_budget
    }
    async fn begin(&self, id: &ExecutionId) -> Result<PostgresSession, StoreError> {
        let mut conn = self.open(READ_COMMITTED).await?;
        let opened = async {
            let c = conn.client();
            let head = self.lock_head(c, id).await?.ok_or(StoreError::NotFound)?;
            // Sample after the lock: now() is the transaction start, which may precede
            // a lock wait and so be earlier than events another session just committed.
            let at_ms: i64 = c
                .query_one(
                    "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                    &[],
                )
                .await
                .map_err(db)?
                .get(0);
            Ok((read_stored(c, head).await?, at_ms))
        }
        .await;
        let (snapshot, at_ms) = match opened {
            Ok(value) => value,
            Err(error) => {
                conn.rollback().await;
                return Err(error);
            }
        };
        Ok(PostgresSession {
            store: self.clone(),
            conn: RefCell::new(Some(conn)),
            snapshot,
            at_ms,
            consumed: Cell::new(false),
        })
    }
}
impl ExecutionStore for PostgresSession {
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        if id != &self.snapshot.projection.execution_id {
            return Err(StoreError::NotFound);
        }
        if self.consumed.get() {
            return Err(StoreError::InvalidBatch("store.session_closed"));
        }
        Ok(self.snapshot.clone())
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        if self.consumed.replace(true) {
            return Err(StoreError::InvalidBatch("store.session_closed"));
        }
        let mut conn = self
            .conn
            .borrow_mut()
            .take()
            .ok_or(StoreError::InvalidBatch("store.session_closed"))?;
        let result = async {
            if batch.execution_id != self.snapshot.projection.execution_id {
                return Err(StoreError::NotFound);
            }
            if batch.expected_sequence != self.snapshot.revision {
                return Ok(CommitResult::Conflict);
            }
            batch.check_transaction_time(self.at_ms, self.snapshot.projection.completed_at_ms)?;
            let head = Head {
                revision: self.snapshot.revision,
                projection: self.snapshot.projection.clone(),
            };
            self.store
                .apply_commit(conn.client(), batch, Some(head), Some(self.at_ms))
                .await
        }
        .await;
        finish_commit(&mut conn, result).await
    }
}
