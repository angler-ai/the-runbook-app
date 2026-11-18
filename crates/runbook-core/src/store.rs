//! Declarative execution-owned writes. The backend transaction is the arbiter.
use runbook_engine::{EventEnvelope, ExecutionPhase, NotificationIntent};
use runbook_model::{
    ClaimId, Data, ExecutionId, ExecutionStatus, RunbookId, StepRunId, StepStatus, Verdict,
    VersionId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub mod memory;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
};

/// Derived rows support reads/indexes. Rebuild authoritative reducer state from
/// the pinned compiled blob and ordered events; this is not a compiler checkpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionProjection {
    pub execution_id: ExecutionId,
    pub runbook_id: RunbookId,
    pub version_id: VersionId,
    pub compiled_hash: [u8; 32],
    /// Engine semantics the run's log was recorded under (F95), taken from
    /// its `ExecutionStarted` event. Heads stored before the marker existed
    /// read as the legacy version.
    #[serde(default = "runbook_engine::legacy_engine_version")]
    pub engine_version: u32,
    pub sequence: u64,
    pub status: ExecutionStatus,
    pub phase: ExecutionPhase,
    /// Adapters index (runbook_id, completed_at_ms) for UTC-day export/retention.
    pub completed_at_ms: Option<i64>,
    #[serde(with = "runbook_spec::stored_value")]
    pub result: Value,
    #[serde(with = "runbook_spec::stored_data")]
    pub outputs: Data,
    pub steps: Vec<StepProjection>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepProjection {
    pub execution_id: ExecutionId,
    pub step_run_id: StepRunId,
    pub path: String,
    pub status: StepStatus,
    pub verdict: Option<Verdict>,
    #[serde(with = "runbook_spec::stored_data")]
    pub outputs: Data,
    pub summary: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerRecord {
    pub execution_id: ExecutionId,
    pub key: String,
    pub generation: u64,
    pub due_at_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimerChange {
    Upsert(TimerRecord),
    Delete {
        execution_id: ExecutionId,
        key: String,
    },
}
impl TimerChange {
    pub fn execution_id(&self) -> &ExecutionId {
        match self {
            Self::Upsert(timer) => &timer.execution_id,
            Self::Delete { execution_id, .. } => execution_id,
        }
    }
    pub fn key(&self) -> &str {
        match self {
            Self::Upsert(timer) => &timer.key,
            Self::Delete { key, .. } => key,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IdempotencyRecord {
    pub execution_id: ExecutionId,
    pub key: String,
    pub request_hash: [u8; 32],
    #[serde(with = "runbook_spec::stored_value")]
    pub response: Value,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimAction {
    Claim,
    Heartbeat,
    Release,
    AdminRelease,
    Expire,
}
/// Session provenance recorded on a claim (D26).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimSession {
    pub principal: String,
    pub actor: runbook_model::ActorRef,
    /// Non-secret credential ID for audit; never a token value.
    pub credential_ref: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimWrite {
    pub execution_id: ExecutionId,
    pub step_run_id: StepRunId,
    pub expected_generation: u64,
    pub claim_id: ClaimId,
    pub generation: u64,
    #[serde(default = "claim_action")]
    pub action: ClaimAction,
    pub owner: runbook_model::ActorRef,
    pub session: ClaimSession,
    pub lease_deadline_at_ms: Option<i64>,
    #[serde(default)]
    pub released: bool,
}
fn claim_action() -> ClaimAction {
    ClaimAction::Claim
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteStage {
    Events,
    Projection,
    Timers,
    Outbox,
    Idempotency,
    Claims,
    Credentials,
    DerivedRows,
    RunDeletion,
}
pub fn idempotent_response(
    run: &StoredExecution,
    key: &str,
    hash: &[u8; 32],
) -> Result<Option<Value>, StoreError> {
    match run.idempotency.get(key) {
        Some(record) if &record.request_hash != hash => {
            Err(StoreError::InvalidBatch("store.idempotency_conflict"))
        }
        Some(record) => Ok(Some(record.response.clone())),
        None => Ok(None),
    }
}
/// Every precondition is decided against expected_sequence. No closures, backend
/// handles, or arbitrary mutations can escape the adapter's atomic commit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionCommit {
    pub execution_id: ExecutionId,
    pub expected_sequence: u64,
    pub events: Vec<EventEnvelope>,
    pub projection: ExecutionProjection,
    pub timers: Vec<TimerChange>,
    pub outbox: Vec<NotificationIntent>,
    #[serde(default)]
    pub idempotency: Vec<IdempotencyRecord>,
    #[serde(default)]
    pub claims: Vec<ClaimWrite>,
    /// Execution credential writes (B7), applied in this transaction.
    #[serde(default)]
    pub credentials: Vec<crate::execution_credentials::CredentialWrite>,
    #[serde(default)]
    pub derived_outbox: Vec<crate::catalog::DerivedUpdate>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredExecution {
    pub revision: u64,
    pub projection: ExecutionProjection,
    pub events: Vec<EventEnvelope>,
    pub timers: BTreeMap<String, TimerRecord>,
    /// Retained across deletion so a timer incarnation cannot be resurrected.
    #[serde(default)]
    pub timer_history: BTreeMap<String, TimerRecord>,
    pub outbox: BTreeMap<String, NotificationIntent>,
    #[serde(default)]
    pub idempotency: BTreeMap<String, IdempotencyRecord>,
    #[serde(default)]
    pub claims: BTreeMap<StepRunId, ClaimWrite>,
    #[serde(default)]
    pub derived_outbox: BTreeMap<u64, crate::catalog::DerivedUpdate>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitResult {
    Committed(u64),
    Conflict,
}
/// CF13: the code of a commit refused because one of its rows would pass
/// what a store keeps in one row ([`MAX_ROW_BYTES`] on Durable Object
/// SQLite and D1). Retrying cannot help, so it is a 422, which clients do
/// not retry, never a retryable `503 storage.unavailable`.
pub const ROW_TOO_LARGE: &str = "execution.row_too_large";
/// Durable Object SQLite's (and D1's) limit on one string, BLOB or row.
pub const MAX_ROW_BYTES: usize = 2_000_000;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    NotFound,
    InvalidBatch(&'static str),
    Backend(&'static str),
}

/// Host implementations may keep backend transaction state internally. Neither
/// the trait's futures nor its implementation require Send. The expected head
/// must be checked inside the same database transaction that writes all rows.
/// Runtime timestamps are supplied by storage; no server clock is sampled here.
pub trait ExecutionStore {
    fn load(&self, id: &ExecutionId) -> impl Future<Output = Result<StoredExecution, StoreError>>;
    fn commit(
        &self,
        batch: ExecutionCommit,
    ) -> impl Future<Output = Result<CommitResult, StoreError>>;
}
impl ExecutionCommit {
    /// Structural validation, shared by adapters. Engine reduction and policy
    /// decisions happen before this boundary; the adapter still checks the head.
    pub fn validate(&self) -> Result<u64, StoreError> {
        let invalid = StoreError::InvalidBatch;
        // A batch without events may only record idempotent replies or
        // release claims (a stranded lease, F6); nothing else can change.
        if self.events.is_empty()
            && ((self.idempotency.is_empty() && self.claims.is_empty())
                || self.claims.iter().any(|claim| !claim.released)
                || !self.timers.is_empty()
                || !self.outbox.is_empty()
                || !self.credentials.is_empty()
                || !self.derived_outbox.is_empty())
        {
            return Err(invalid("store.empty_batch"));
        }
        let mut sequence = self.expected_sequence;
        for event in &self.events {
            sequence = sequence
                .checked_add(1)
                .ok_or(invalid("store.sequence_overflow"))?;
            if event.execution_id != self.execution_id || event.seq != sequence {
                return Err(invalid("store.invalid_event_sequence"));
            }
        }
        if self.projection.execution_id != self.execution_id || self.projection.sequence != sequence
        {
            return Err(invalid("store.invalid_projection"));
        }
        let completed = matches!(
            self.projection.status,
            ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
        );
        if completed != self.projection.completed_at_ms.is_some() {
            return Err(invalid("store.invalid_completion_time"));
        }
        let mut step_ids = BTreeSet::new();
        for step in &self.projection.steps {
            if step.execution_id != self.execution_id || !step_ids.insert(&step.step_run_id) {
                return Err(invalid("store.invalid_step_projection"));
            }
        }
        let mut timer_keys = BTreeSet::new();
        for change in &self.timers {
            if change.execution_id() != &self.execution_id
                || change.key().is_empty()
                || !timer_keys.insert(change.key())
                || matches!(change,TimerChange::Upsert(t) if t.generation == 0)
            {
                return Err(invalid("store.invalid_timer"));
            }
        }
        let mut event_ids = BTreeSet::new();
        for intent in &self.outbox {
            if intent.execution_id != self.execution_id
                || intent.sequence <= self.expected_sequence
                || intent.sequence > sequence
                || intent.event_id.is_empty()
                || !event_ids.insert(&intent.event_id)
            {
                return Err(invalid("store.invalid_outbox"));
            }
        }
        let mut keys = BTreeSet::new();
        for row in &self.idempotency {
            if row.execution_id != self.execution_id || row.key.is_empty() || !keys.insert(&row.key)
            {
                return Err(invalid("store.invalid_idempotency"));
            }
        }
        let mut claims = BTreeSet::new();
        for row in &self.claims {
            let generation_ok = match row.action {
                ClaimAction::Claim => {
                    row.expected_generation.checked_add(1) == Some(row.generation) && !row.released
                }
                ClaimAction::Heartbeat => {
                    row.generation == row.expected_generation && !row.released
                }
                ClaimAction::AdminRelease => {
                    row.expected_generation.checked_add(1) == Some(row.generation) && row.released
                }
                ClaimAction::Release | ClaimAction::Expire => {
                    row.generation == row.expected_generation && row.released
                }
            };
            if row.execution_id != self.execution_id
                || !generation_ok
                || !claims.insert(&row.step_run_id)
            {
                return Err(invalid("store.invalid_claim"));
            }
        }
        let mut derived_sequences = BTreeSet::new();
        for update in &self.derived_outbox {
            let row = &update.summary;
            if row.execution_id != self.execution_id
                || row.runbook_id != self.projection.runbook_id
                || row.sequence <= self.expected_sequence
                || row.sequence != sequence
                || row.status != self.projection.status
                || row.completed_at_ms != self.projection.completed_at_ms
                || update.release_concurrency != completed
                || (completed && !update.assignments.is_empty())
                || !derived_sequences.insert(row.sequence)
                || update
                    .assignments
                    .iter()
                    .any(|a| a.execution_id != self.execution_id)
                || update
                    .memory
                    .iter()
                    .any(|proposal| !crate::memory_entries::proposal_fits(proposal, self, sequence))
            {
                return Err(invalid("store.invalid_derived_update"));
            }
        }
        Ok(sequence)
    }

    /// Session fence: every timestamp in the batch must be the transaction
    /// time the session supplied before evaluation, never a process clock.
    /// Session time fence: every new event and intent is stamped now.
    /// A terminal completion time may also be the previously stored one,
    /// so completed runs can be amended without moving completed_at (B9a).
    pub fn check_transaction_time(
        &self,
        at_ms: i64,
        previous_completed_at_ms: Option<i64>,
    ) -> Result<(), StoreError> {
        let completed_ok = match self.projection.completed_at_ms {
            None => true,
            Some(time) => time == at_ms || Some(time) == previous_completed_at_ms,
        };
        for record in &self.idempotency {
            if crate::actor::replay_record_parts(record)?.is_some_and(|(_, at)| at != at_ms) {
                return Err(StoreError::InvalidBatch("store.transaction_time_mismatch"));
            }
        }
        if self.events.iter().any(|e| e.at_ms != at_ms)
            || self.outbox.iter().any(|e| e.at_ms != at_ms)
            || (!self.events.is_empty() && !completed_ok)
        {
            return Err(StoreError::InvalidBatch("store.transaction_time_mismatch"));
        }
        Ok(())
    }
}
