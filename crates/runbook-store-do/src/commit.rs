//! `ExecutionStore` and the session/commit path. One commit is one storage
//! transaction: the head check, `validate()`, every row and the re-armed
//! alarm succeed together or not at all. The rules mirror the in-memory
//! and Postgres adapters; the derived outbox is recorded here and applied
//! to D1 later (CF3), never in this transaction.
use crate::{
    DoSql, DoStore, Fault,
    sql::{Columns, Value, decode, encode, int, uint},
};
use runbook_core::{
    ClaimWrite, CommitResult, ExecutionCommit, ExecutionProjection, ExecutionStore,
    IdempotencyRecord, StoreError, StoredExecution, TimerChange, TimerRecord, WriteStage,
    actor::replay_record_parts,
    session::{ExecutionSession, SessionStore},
};
use runbook_engine::{EventEnvelope, NotificationIntent};
use runbook_model::ExecutionId;
use std::{cell::Cell, collections::BTreeMap};

/// Actor replay records are kept this long (as native).
const REPLAY_WINDOW_MS: i64 = 86_400_000;

fn invalid(code: &'static str) -> StoreError {
    StoreError::InvalidBatch(code)
}

fn timers(
    rows: Vec<crate::Row>,
    id: &ExecutionId,
) -> Result<BTreeMap<String, TimerRecord>, StoreError> {
    rows.into_iter()
        .map(|row| {
            let key = row.text(0)?.to_owned();
            Ok((
                key.clone(),
                TimerRecord {
                    execution_id: id.clone(),
                    key,
                    generation: row.unsigned(1)?,
                    due_at_ms: row.integer(2)?,
                },
            ))
        })
        .collect()
}

/// The event type tag, for the `event.type` column.
pub(crate) fn event_type(envelope: &EventEnvelope) -> String {
    serde_json::to_value(&envelope.event)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned))
        .unwrap_or_default()
}

pub(crate) fn status_text<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    match serde_json::to_value(value).map_err(|_| StoreError::Backend("do.encode"))? {
        serde_json::Value::String(text) => Ok(text),
        other => Ok(other.to_string()),
    }
}

impl<B: DoSql> DoStore<B> {
    /// This execution's state for a load or a session snapshot: the head,
    /// the event log, live timers and their history, claims, idempotency
    /// rows, undelivered intents, and undelivered derived rows plus the
    /// latest one. Delivered outbox rows stay stored but are not loaded.
    pub(crate) fn load_in(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        let head = self.owned_head(id)?;
        let sql = &self.sql;
        let (projection, _) = self.read_projection()?;
        let events = sql
            .query("SELECT envelope FROM event ORDER BY seq", &[])?
            .iter()
            .map(|row| decode(row.text(0)?))
            .collect::<Result<_, _>>()?;
        // Delivered intents are done; an undelivered one may still matter.
        let outbox = sql
            .query(
                "SELECT event_id, intent FROM outbox WHERE acked_at_ms IS NULL",
                &[],
            )?
            .iter()
            .map(|row| Ok((row.text(0)?.to_owned(), decode(row.text(1)?)?)))
            .collect::<Result<_, StoreError>>()?;
        // Undelivered derived rows, and the latest row even once delivered:
        // the actor diffs inbox assignments against it.
        let derived_outbox = sql
            .query(
                "SELECT sequence, entry FROM derived_outbox WHERE acked_at_ms IS NULL \
                 OR sequence = (SELECT MAX(sequence) FROM derived_outbox)",
                &[],
            )?
            .iter()
            .map(|row| {
                let sequence = row.unsigned(0)?;
                Ok((sequence, self.read_derived(sequence, row.text(1)?)?))
            })
            .collect::<Result<_, StoreError>>()?;
        let idempotency = sql
            .query("SELECT key, body FROM idempotency", &[])?
            .iter()
            .map(|row| Ok((row.text(0)?.to_owned(), decode(row.text(1)?)?)))
            .collect::<Result<_, StoreError>>()?;
        let claims = sql
            .query("SELECT body FROM claim", &[])?
            .iter()
            .map(|row| {
                let claim: ClaimWrite = decode(row.text(0)?)?;
                Ok((claim.step_run_id.clone(), claim))
            })
            .collect::<Result<_, StoreError>>()?;
        Ok(StoredExecution {
            revision: head.revision,
            projection,
            events,
            timers: timers(
                sql.query("SELECT key, generation, due_at_ms FROM timer", &[])?,
                id,
            )?,
            timer_history: timers(
                sql.query("SELECT key, generation, due_at_ms FROM timer_history", &[])?,
                id,
            )?,
            outbox,
            idempotency,
            claims,
            derived_outbox,
        })
    }

    /// Create this object's head at revision 0, with no events yet (the
    /// conformance `create`, and the head a start initializes).
    pub fn create_execution(&self, projection: ExecutionProjection) -> Result<(), StoreError> {
        self.write(move |store| store.insert_head(&projection))
    }

    pub(crate) fn insert_head(&self, projection: &ExecutionProjection) -> Result<(), StoreError> {
        self.named(&projection.execution_id)?;
        if self.head()?.is_some() {
            return Err(invalid("store.execution_exists"));
        }
        if projection.sequence != 0 {
            return Err(invalid("store.invalid_projection"));
        }
        self.write_projection(&projection.execution_id, 0, projection, None)?;
        self.bump_writes()
    }

    /// One execution commit inside an open transaction, at transaction time
    /// `now`: the store clock for a direct commit, or a session's `begin`
    /// time (actor logic awaits D1 and R2 between the two, CF0).
    pub(crate) fn commit_in(
        &self,
        batch: ExecutionCommit,
        now: i64,
    ) -> Result<CommitResult, StoreError> {
        let id = batch.execution_id.clone();
        self.named(&id)?;
        // Read the head and only the rows the checks below name: a commit
        // never loads the whole run.
        let revision = match self.head()? {
            Some(head) if head.scope != self.scope || head.execution_id != id => {
                return Err(StoreError::NotFound);
            }
            Some(head) => Some(head.revision),
            None => None,
        };
        if revision.unwrap_or(0) != batch.expected_sequence {
            return Ok(CommitResult::Conflict);
        }
        let sequence = batch.validate()?;
        let stored = match revision {
            Some(_) => Some(self.read_projection()?),
            None => None,
        };
        let previous = stored.as_ref().map(|(projection, _)| projection);
        if batch.events.is_empty() && previous != Some(&batch.projection) {
            return Err(invalid("store.replay_projection_changed"));
        }
        if let Some(previous) = previous
            && (previous.runbook_id != batch.projection.runbook_id
                || previous.version_id != batch.projection.version_id
                || previous.compiled_hash != batch.projection.compiled_hash)
        {
            return Err(invalid("store.pinned_version_changed"));
        }
        for intent in &batch.outbox {
            if self.exists(
                "SELECT 1 FROM outbox WHERE event_id = ?",
                intent.event_id.as_str(),
            )? {
                return Err(invalid("store.duplicate_outbox"));
            }
        }
        for change in &batch.timers {
            if let TimerChange::Upsert(timer) = change {
                let live = self.timer_row("timer", &timer.key, &id)?;
                let prior = match self.timer_row("timer_history", &timer.key, &id)? {
                    Some(history) => Some(history),
                    None => live.clone(),
                };
                if let Some(prior) = prior
                    && (timer.generation < prior.generation
                        || (timer.generation == prior.generation && live.as_ref() != Some(timer)))
                {
                    return Err(invalid("store.timer_generation_reused"));
                }
            }
        }
        for change in &batch.timers {
            if let TimerChange::Upsert(timer) = change {
                crate::queue::alarm_time(timer.due_at_ms, "store.timer_out_of_range")?;
            }
        }
        for claim in &batch.claims {
            if let Some(deadline) = claim.lease_deadline_at_ms {
                crate::queue::alarm_time(deadline, "store.time_out_of_range")?;
            }
            let generation = self
                .sql
                .query(
                    "SELECT generation FROM claim WHERE step_run_id = ?",
                    &[claim.step_run_id.as_str().into()],
                )?
                .first()
                .map(|row| row.unsigned(0))
                .transpose()?
                .unwrap_or(0);
            if generation != claim.expected_generation {
                return Ok(CommitResult::Conflict);
            }
        }
        for record in &batch.idempotency {
            if self.exists("SELECT 1 FROM idempotency WHERE key = ?", &record.key)? {
                return Err(invalid("store.idempotency_conflict"));
            }
        }
        for write in &batch.credentials {
            self.precheck_credential(&id, write)?;
        }
        for write in &batch.credentials {
            self.apply_credential(&id, write)?;
        }
        self.checkpoint(WriteStage::Credentials)?;
        for record in &batch.idempotency {
            if replay_record_parts(record)?.is_some() {
                self.write_replay(&id, record, now)?;
            }
        }
        for event in &batch.events {
            self.put(
                "INSERT INTO event (org_id, project_id, execution_id, seq, at_ms, type, envelope) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                &self.owned(
                    &id,
                    vec![
                        uint(event.seq)?,
                        int(event.at_ms)?,
                        event_type(event).into(),
                        encode(event)?,
                    ],
                ),
            )?;
        }
        self.checkpoint(WriteStage::Events)?;
        if self.take_fault(Fault::Panic) {
            panic!("runbook-store-do: injected panic mid-commit");
        }
        // CF13: a split head's unchanged step rows stay as they are.
        let unchanged = match &stored {
            Some((projection, crate::rows::HeadShape::Split)) => Some(&projection.steps[..]),
            _ => None,
        };
        self.write_projection(&id, sequence, &batch.projection, unchanged)?;
        self.bump_writes()?;
        self.checkpoint(WriteStage::Projection)?;
        for change in &batch.timers {
            match change {
                TimerChange::Upsert(timer) => {
                    for table in ["timer", "timer_history"] {
                        self.sql.exec(
                            &format!(
                                "INSERT INTO {table} (org_id, project_id, execution_id, key, \
                                 generation, due_at_ms) VALUES (?, ?, ?, ?, ?, ?) \
                                 ON CONFLICT (key) DO UPDATE SET generation = excluded.generation, \
                                 due_at_ms = excluded.due_at_ms"
                            ),
                            &self.owned(
                                &id,
                                vec![
                                    timer.key.as_str().into(),
                                    uint(timer.generation)?,
                                    int(timer.due_at_ms)?,
                                ],
                            ),
                        )?;
                    }
                }
                TimerChange::Delete { key, .. } => {
                    // The history row already holds the live timer (every
                    // upsert writes both), so deleting keeps its incarnation.
                    self.sql
                        .exec("DELETE FROM timer WHERE key = ?", &[key.as_str().into()])?;
                }
            }
        }
        self.checkpoint(WriteStage::Timers)?;
        for intent in &batch.outbox {
            self.insert_intent(&id, intent)?;
        }
        if self.take_fault(Fault::SqlConstraint) {
            // Re-insert an event this transaction already wrote: a genuine
            // SQLite PRIMARY KEY failure, not a Rust-side error.
            let seq = batch.events.first().map_or(1, |event| event.seq);
            self.sql.exec(
                "INSERT INTO event (org_id, project_id, execution_id, seq, at_ms, type, envelope) \
                 VALUES (?, ?, ?, ?, 0, 'duplicate', '{}')",
                &self.owned(&id, vec![uint(seq)?]),
            )?;
        }
        self.checkpoint(WriteStage::Outbox)?;
        for record in &batch.idempotency {
            if replay_record_parts(record)?.is_none() {
                self.put(
                    "INSERT INTO idempotency (org_id, project_id, execution_id, key, \
                     request_hash, body) VALUES (?, ?, ?, ?, ?, ?)",
                    &self.owned(
                        &id,
                        vec![
                            record.key.as_str().into(),
                            record.request_hash.to_vec().into(),
                            encode(record)?,
                        ],
                    ),
                )?;
            }
        }
        self.checkpoint(WriteStage::Idempotency)?;
        for claim in &batch.claims {
            self.write_claim(&id, claim)?;
        }
        self.checkpoint(WriteStage::Claims)?;
        for update in &batch.derived_outbox {
            self.write_derived(&id, update.summary.sequence, now, update)?;
        }
        self.checkpoint(WriteStage::DerivedRows)?;
        let completed = matches!(
            batch.projection.status,
            runbook_model::ExecutionStatus::Succeeded
                | runbook_model::ExecutionStatus::Failed
                | runbook_model::ExecutionStatus::Cancelled
        );
        if completed {
            // Closing (or amending a closed run) revokes every credential
            // family at the run's last event time, in this transaction.
            let closed_at = match batch.events.last() {
                Some(event) => event.at_ms,
                None => self
                    .sql
                    .query("SELECT at_ms FROM event ORDER BY seq DESC LIMIT 1", &[])?
                    .first()
                    .map(|row| row.integer(0))
                    .transpose()?
                    .unwrap_or(0),
            };
            self.revoke_all_credentials(&id, closed_at)?;
        }
        Ok(CommitResult::Committed(sequence))
    }

    fn exists(&self, sql: &str, key: &str) -> Result<bool, StoreError> {
        Ok(!self.sql.query(sql, &[key.into()])?.is_empty())
    }

    fn timer_row(
        &self,
        table: &str,
        key: &str,
        id: &ExecutionId,
    ) -> Result<Option<TimerRecord>, StoreError> {
        Ok(timers(
            self.sql.query(
                &format!("SELECT key, generation, due_at_ms FROM {table} WHERE key = ?"),
                &[key.into()],
            )?,
            id,
        )?
        .into_values()
        .next())
    }

    pub(crate) fn insert_intent(
        &self,
        id: &ExecutionId,
        intent: &NotificationIntent,
    ) -> Result<(), StoreError> {
        self.put(
            "INSERT INTO outbox (org_id, project_id, execution_id, event_id, sequence, at_ms, \
             kind, intent) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            &self.owned(
                id,
                vec![
                    intent.event_id.as_str().into(),
                    uint(intent.sequence)?,
                    int(intent.at_ms)?,
                    intent.kind.as_str().into(),
                    encode(intent)?,
                ],
            ),
        )
    }

    pub(crate) fn write_claim(
        &self,
        id: &ExecutionId,
        claim: &ClaimWrite,
    ) -> Result<(), StoreError> {
        self.put(
            "INSERT INTO claim (org_id, project_id, execution_id, step_run_id, claim_id, \
             generation, released, lease_deadline_at_ms, body) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (step_run_id) DO UPDATE SET claim_id = excluded.claim_id, \
             generation = excluded.generation, released = excluded.released, \
             lease_deadline_at_ms = excluded.lease_deadline_at_ms, body = excluded.body",
            &self.owned(
                id,
                vec![
                    claim.step_run_id.as_str().into(),
                    claim.claim_id.as_str().into(),
                    uint(claim.generation)?,
                    claim.released.into(),
                    claim
                        .lease_deadline_at_ms
                        .map(int)
                        .transpose()?
                        .unwrap_or(Value::Null),
                    encode(claim)?,
                ],
            ),
        )
    }

    /// Record one actor replay row at the store clock, pruning this
    /// execution's rows older than the replay window.
    pub(crate) fn write_replay(
        &self,
        id: &ExecutionId,
        record: &IdempotencyRecord,
        now: i64,
    ) -> Result<(), StoreError> {
        let (prefix, at) = replay_record_parts(record)?.ok_or(invalid("replay.invalid_record"))?;
        if at != now {
            return Err(invalid("store.transaction_time_mismatch"));
        }
        self.sql.exec(
            "DELETE FROM actor_replay WHERE at_ms <= ?",
            &[int(now.saturating_sub(REPLAY_WINDOW_MS))?],
        )?;
        let taken = self.sql.query(
            "SELECT 1 FROM actor_replay WHERE prefix = ? AND at_ms = ?",
            &[prefix.into(), int(at)?],
        )?;
        if !taken.is_empty() {
            return Err(invalid("store.idempotency_conflict"));
        }
        self.put(
            "INSERT INTO actor_replay (org_id, project_id, execution_id, prefix, at_ms, body) \
             VALUES (?, ?, ?, ?, ?, ?)",
            &self.owned(id, vec![prefix.into(), int(at)?, encode(record)?]),
        )
    }

    /// The newest replay row under `prefix` still inside the window.
    pub(crate) fn lookup_replay(
        &self,
        prefix: &str,
        now: i64,
    ) -> Result<Option<IdempotencyRecord>, StoreError> {
        self.sql
            .query(
                "SELECT body FROM actor_replay WHERE prefix = ? AND at_ms > ? \
                 ORDER BY at_ms DESC LIMIT 1",
                &[prefix.into(), int(now.saturating_sub(REPLAY_WINDOW_MS))?],
            )?
            .first()
            .map(|row| decode(row.text(0)?))
            .transpose()
    }
}

impl<B: DoSql> ExecutionStore for DoStore<B> {
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        self.load_in(id)
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        self.commit_at(batch, None, None)
    }
}

impl<B: DoSql> DoStore<B> {
    /// Commit at `at_ms`, or at the store clock when `None`.
    /// With `fence`, the commit is a session's: it conflicts unless no
    /// write moved `meta.writes` since the session began.
    pub(crate) fn commit_at(
        &self,
        batch: ExecutionCommit,
        at_ms: Option<i64>,
        fence: Option<u64>,
    ) -> Result<CommitResult, StoreError> {
        self.write(move |store| {
            if let Some(writes) = fence
                && store.head()?.map(|head| head.writes) != Some(writes)
            {
                return Ok(CommitResult::Conflict);
            }
            let now = at_ms.unwrap_or_else(|| store.sql.now_ms());
            let result = store.commit_in(batch, now)?;
            if store.take_fault(Fault::AfterAlarm) {
                // The alarm is re-armed by `write` after this body; arm it
                // here first so the fault provably follows an alarm write.
                crate::queue::rearm(store)?;
                return Err(StoreError::Backend("injected_after_alarm"));
            }
            Ok(result)
        })
    }
}

/// A session: the object's gate held from `begin` (no other session runs
/// meanwhile, as behind Postgres's row lock), a read snapshot, the store
/// clock sampled at `begin`, and the `meta.writes` fence its commit checks,
/// so a write the gate does not cover (a plain commit, a heartbeat) since
/// `begin` turns the commit into a conflict. The gate is released when the
/// session commits, is released or is dropped. A DO
/// runs one request's synchronous work at a time, so no lock is held.
pub struct DoSession<B: DoSql> {
    store: DoStore<B>,
    snapshot: StoredExecution,
    at_ms: i64,
    writes: u64,
    consumed: Cell<bool>,
    held: std::cell::RefCell<Option<crate::gate::Held>>,
}
impl<B: DoSql> ExecutionSession for DoSession<B> {
    fn transaction_time_ms(&self) -> i64 {
        self.at_ms
    }
    async fn actor_replay(&self, prefix: &str) -> Result<Option<IdempotencyRecord>, StoreError> {
        if self.consumed.get() {
            return Err(invalid("store.session_closed"));
        }
        self.store
            .owned_head(&self.snapshot.projection.execution_id)?;
        self.store.lookup_replay(prefix, self.at_ms)
    }
    async fn release(&self) {
        self.consumed.set(true);
        self.held.borrow_mut().take();
    }
}
impl<B: DoSql> ExecutionStore for DoSession<B> {
    async fn load(&self, id: &ExecutionId) -> Result<StoredExecution, StoreError> {
        if id != &self.snapshot.projection.execution_id {
            return Err(StoreError::NotFound);
        }
        if self.consumed.get() {
            return Err(invalid("store.session_closed"));
        }
        Ok(self.snapshot.clone())
    }
    async fn commit(&self, batch: ExecutionCommit) -> Result<CommitResult, StoreError> {
        if self.consumed.replace(true) {
            return Err(invalid("store.session_closed"));
        }
        // Whatever happens next, this session is over: let the next one in.
        let _held = self.held.borrow_mut().take();
        if batch.execution_id != self.snapshot.projection.execution_id {
            return Err(StoreError::NotFound);
        }
        if batch.expected_sequence != self.snapshot.revision {
            return Ok(CommitResult::Conflict);
        }
        batch.check_transaction_time(self.at_ms, self.snapshot.projection.completed_at_ms)?;
        self.store
            .commit_at(batch, Some(self.at_ms), Some(self.writes))
    }
}
impl<B: DoSql> SessionStore for DoStore<B> {
    type Session = DoSession<B>;
    async fn begin(&self, id: &ExecutionId) -> Result<Self::Session, StoreError> {
        self.owned_head(id)?;
        let held = crate::gate::enter(self.object.clone()).await;
        // Read after the gate: the previous session's commit is visible.
        let head = self.owned_head(id)?;
        Ok(DoSession {
            store: self.clone(),
            snapshot: self.load_in(id)?,
            at_ms: self.sql.now_ms(),
            writes: head.writes,
            consumed: Cell::new(false),
            held: std::cell::RefCell::new(Some(held)),
        })
    }
}
