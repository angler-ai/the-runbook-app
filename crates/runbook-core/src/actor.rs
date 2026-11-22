//! Actor runtime (B6a): one execution session per actor operation.
//! Events are appended and settled through the engine decider, then the
//! whole batch — events, projection, timers, outbox, claims and the
//! derived summary — commits atomically through the session.
mod replay;
use crate::{
    ExecutionCommit, ExecutionStore, StoreError, StoredExecution,
    blob::{BlobKey, BlobStore},
    catalog::{DerivedUpdate, TenantScope},
    directory::DirectoryStore,
    session::{ExecutionSession, SessionStore},
    start::{StartRecord, StartStore, summary_of},
};
pub use replay::{Replay, replay_record_parts};
use runbook_engine::{EngineError, Event, ExecutionState, apply};
use runbook_model::{ActorKind, ActorRef, ExecutionId, FieldError, UserId};
use std::rc::Rc;

/// Verified actor facts from the credential layer (never request JSON).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActorPrincipal {
    pub principal: String,
    pub actor: ActorRef,
    pub credential_ref: String,
}
impl ActorPrincipal {
    pub fn kind(&self) -> Option<ActorKind> {
        match &self.actor {
            ActorRef::Human { .. } => Some(ActorKind::Human),
            ActorRef::Agent { .. } => Some(ActorKind::Agent),
            ActorRef::System => None,
        }
    }
    pub fn user_id(&self) -> Option<&UserId> {
        match &self.actor {
            ActorRef::Human { user_id } => Some(user_id),
            _ => None,
        }
    }
    pub fn session(&self) -> crate::ClaimSession {
        crate::ClaimSession {
            principal: self.principal.clone(),
            actor: self.actor.clone(),
            credential_ref: self.credential_ref.clone(),
        }
    }
}

#[derive(Debug)]
pub enum ActorError {
    Store(StoreError),
    Engine(EngineError),
    /// Caller-visible rejection (claim fences, pause, validation):
    /// routes map the code and field errors to a Problem (B6b).
    Rejected {
        code: &'static str,
        errors: Vec<FieldError>,
    },
}
impl ActorError {
    pub fn rejected(code: &'static str) -> Self {
        Self::Rejected {
            code,
            errors: vec![],
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::Store(StoreError::NotFound) => "execution.not_found",
            Self::Store(StoreError::InvalidBatch(code)) => code,
            Self::Store(StoreError::Backend(_)) => "store.backend",
            Self::Engine(error) => error.code(),
            Self::Rejected { code, .. } => code,
        }
    }
}
impl From<StoreError> for ActorError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<EngineError> for ActorError {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}
impl std::fmt::Display for ActorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ActorError {}

pub struct ActorWork<S: SessionStore> {
    pub(crate) session: S::Session,
    pub snapshot: StoredExecution,
    pub record: StartRecord,
    pub runtime: crate::runtime::Runtime,
    /// D40 proposals recorded by this operation; they ride the commit's
    /// derived row so the catalog applies them with the execution.
    pub(crate) memory: Vec<crate::memory_entries::MemoryProposal>,
    /// CF13 review round 1: the store's run budget, and the stored log's
    /// bytes once counted ([`crate::run_budget`]).
    run_budget: u64,
    logged: Option<u64>,
    /// R0-4: the agent version a runner's claim pins (set by
    /// `grants::runner_claim` only).
    pub(crate) runner_pin: Option<String>,
}

/// Open the execution session first (D28), then read the directory
/// snapshot votes will be authorized against, then rebuild state by
/// replaying the stored events over the pinned snapshot.
pub async fn begin_actor<S>(
    store: &S,
    directory: &impl DirectoryStore,
    scope: TenantScope,
    id: &ExecutionId,
) -> Result<ActorWork<S>, ActorError>
where
    S: StartStore + BlobStore + SessionStore,
{
    let session = store.begin(id).await?;
    let snapshot = session.load(id).await?;
    let directory_snapshot = directory
        .snapshot(&scope.org_id)
        .await
        .map_err(|_| StoreError::Backend("actor.directory_unavailable"))?;
    rebuild(store, session, snapshot, directory_snapshot, scope, id).await
}

/// LANG3: rebuild an execution as `begin_actor` does, but release the
/// session (its lock and connection) as soon as the snapshot and the
/// transaction time are read, and use a directory snapshot the caller read
/// once for all the runs it prepares. The work can only produce a commit
/// for another transaction to apply (`into_commit`), fenced by its
/// sequence.
pub(crate) async fn begin_detached<S>(
    store: &S,
    directory_snapshot: crate::directory::DirectorySnapshot,
    scope: TenantScope,
    id: &ExecutionId,
) -> Result<ActorWork<S>, ActorError>
where
    S: StartStore + BlobStore + SessionStore,
{
    let session = store.begin(id).await?;
    let snapshot = session.load(id).await?;
    session.release().await;
    rebuild(store, session, snapshot, directory_snapshot, scope, id).await
}

async fn rebuild<S>(
    store: &S,
    session: S::Session,
    snapshot: StoredExecution,
    directory_snapshot: crate::directory::DirectorySnapshot,
    scope: TenantScope,
    id: &ExecutionId,
) -> Result<ActorWork<S>, ActorError>
where
    S: StartStore + BlobStore + SessionStore,
{
    let record = store.start_record(id).await?.ok_or(StoreError::NotFound)?;
    let bytes = store
        .get(&BlobKey::Shared {
            hash: record.snapshot_digest,
        })
        .await?;
    let compiled = runbook_spec::restore(&bytes)
        .map_err(|_| StoreError::Backend("start.snapshot_restore_failed"))?;
    let mut state = ExecutionState::new(id.clone(), Rc::new(compiled));
    for envelope in &snapshot.events {
        state = apply(&state, envelope)?;
    }
    let at_ms = session.transaction_time_ms();
    Ok(ActorWork {
        run_budget: store.run_budget(),
        logged: None,
        runtime: crate::runtime::Runtime::new(
            state,
            directory_snapshot,
            scope,
            at_ms,
            crate::step_lookup::ExecutionStepIds::new(id, snapshot.revision),
        ),
        session,
        snapshot,
        record,
        memory: vec![],
        runner_pin: None,
    })
}

impl<S: SessionStore> ActorWork<S> {
    pub fn at_ms(&self) -> i64 {
        self.runtime.at_ms
    }
    pub fn step_index(&self, step_run_id: &runbook_model::StepRunId) -> Result<usize, ActorError> {
        self.runtime.step_index(step_run_id)
    }
    /// Append a request's (or the system's) event. A request's event that
    /// carries data past the run budget is refused (CF13 review round 1,
    /// [`crate::run_budget`]); the system's never is.
    pub fn append(&mut self, event: Event, actor: ActorRef) -> Result<(), ActorError> {
        if actor != ActorRef::System {
            let bytes = crate::run_budget::event_bytes(&event);
            if bytes > crate::run_budget::SMALL_WRITE_BYTES {
                let logged = *self
                    .logged
                    .get_or_insert_with(|| crate::run_budget::log_bytes(&self.snapshot.events));
                let pending = crate::run_budget::log_bytes(self.runtime.pending_events());
                if !crate::run_budget::admits(
                    self.run_budget,
                    logged.saturating_add(pending),
                    bytes,
                ) {
                    return Err(ActorError::rejected(crate::run_budget::RUN_TOO_LARGE));
                }
            }
        }
        self.runtime.append(event, actor)
    }
    /// Settle to quiescence. An open engine version 6 run is held to the
    /// hard ceiling on its log (CF13b, [`crate::run_budget::run_ceiling`]):
    /// past it the run is faulted and closes in this settle.
    pub fn settle(&mut self) -> Result<(), ActorError> {
        if runbook_engine::run_ceiling_applies(&self.runtime.state) {
            let stored = *self
                .logged
                .get_or_insert_with(|| crate::run_budget::log_bytes(&self.snapshot.events));
            self.runtime
                .hold_to_ceiling(stored, crate::run_budget::run_ceiling(self.run_budget));
        }
        self.runtime.settle()
    }
    pub fn has_pending(&self) -> bool {
        self.runtime.has_pending()
    }

    pub async fn replayed<T: serde::de::DeserializeOwned>(
        &self,
        principal: &ActorPrincipal,
        replay: Option<&Replay>,
        operation: &str,
    ) -> Result<Option<T>, ActorError> {
        let Some(replay) = replay else {
            return Ok(None);
        };
        if replay.principal != principal.principal {
            return Err(ActorError::rejected("auth.forbidden"));
        }
        let row = self
            .session
            .actor_replay(&replay.prefix(operation)?)
            .await?;
        Ok(replay.lookup(operation, row.iter(), self.at_ms())?)
    }
    pub(crate) async fn commit_replay<T: serde::Serialize>(
        self,
        claims: Vec<crate::ClaimWrite>,
        replay: Option<&Replay>,
        operation: &str,
        value: &T,
    ) -> Result<crate::CommitResult, ActorError> {
        let records = replay
            .map(|r| r.record(operation, &self.runtime.state.id, self.at_ms(), value))
            .transpose()?
            .into_iter()
            .collect();
        self.commit_records(claims, records).await
    }
    /// Commit everything appended since begin as one execution commit.
    pub(crate) async fn commit(
        self,
        claims: Vec<crate::ClaimWrite>,
    ) -> Result<crate::CommitResult, ActorError> {
        self.commit_records(claims, vec![]).await
    }
    async fn commit_records(
        mut self,
        claims: Vec<crate::ClaimWrite>,
        idempotency: Vec<crate::IdempotencyRecord>,
    ) -> Result<crate::CommitResult, ActorError> {
        let batch = self.batch(claims, idempotency);
        Ok(self.session.commit(batch).await?)
    }
    /// LANG3: the commit this work would make, for another transaction to
    /// apply as a plain CAS commit (fenced by `expected_sequence`). The
    /// session is closed without writing.
    pub(crate) fn into_commit(mut self) -> ExecutionCommit {
        self.batch(vec![], vec![])
    }
    fn batch(
        &mut self,
        claims: Vec<crate::ClaimWrite>,
        idempotency: Vec<crate::IdempotencyRecord>,
    ) -> ExecutionCommit {
        // CF13 review round 1: the stored log was replayed at begin and is
        // not read again; free it before the commit's own allocations (on
        // Workers, the request that ships the commit to the object).
        drop(std::mem::take(&mut self.snapshot.events));
        let replay_only = self.runtime.pending_events().is_empty();
        let mut claims = claims;
        if !replay_only {
            claims.extend(stranded_claims(
                &self.snapshot,
                &self.runtime.state,
                &claims,
            ));
        }
        let projection = if replay_only {
            self.snapshot.projection.clone()
        } else {
            // The stored step rows are superseded by the new projection.
            drop(std::mem::take(&mut self.snapshot.projection.steps));
            crate::start::project_preserving(
                &self.runtime.state,
                self.record.runbook_id.clone(),
                self.record.version_id.clone(),
                self.snapshot.projection.completed_at_ms,
            )
        };
        let summary = summary_of(&self.runtime.state, &projection);
        let mut current_claims = self.snapshot.claims.clone();
        for claim in &claims {
            current_claims.insert(claim.step_run_id.clone(), claim.clone());
        }
        let previous = self
            .snapshot
            .derived_outbox
            .values()
            .next_back()
            .map(|update| update.assignments.as_slice())
            .unwrap_or_default();
        let assignments = crate::runtime::assignments(
            &self.runtime.state,
            &current_claims,
            self.runtime.state.sequence,
            previous,
        );
        let timers = if replay_only {
            vec![]
        } else {
            self.runtime.timer_changes(&self.snapshot)
        };
        ExecutionCommit {
            execution_id: self.runtime.state.id.clone(),
            expected_sequence: self.snapshot.revision,
            events: self.runtime.pending_events().to_vec(),
            projection,
            timers,
            outbox: self.runtime.intents().to_vec(),
            idempotency,
            claims,
            credentials: vec![],
            derived_outbox: if replay_only {
                vec![]
            } else {
                vec![DerivedUpdate {
                    summary,
                    assignments,
                    release_concurrency: matches!(
                        self.runtime.state.status,
                        runbook_model::ExecutionStatus::Succeeded
                            | runbook_model::ExecutionStatus::Failed
                            | runbook_model::ExecutionStatus::Cancelled
                    ),
                    memory: std::mem::take(&mut self.memory),
                }]
            },
        }
    }
}

/// Unreleased claims whose step this commit moves out of `Claimed` without
/// its own claim write (cancel, cleanup, a failed sibling, a timeout): they
/// are released in the same commit, so a lease never outlives its step and
/// heartbeats and log appends for that step stop (F6).
fn stranded_claims(
    snapshot: &StoredExecution,
    state: &ExecutionState,
    written: &[crate::ClaimWrite],
) -> Vec<crate::ClaimWrite> {
    snapshot
        .claims
        .values()
        .filter(|claim| !claim.released)
        .filter(|claim| !written.iter().any(|w| w.step_run_id == claim.step_run_id))
        .filter(|claim| {
            !state.steps.iter().any(|step| {
                step.id.as_ref() == Some(&claim.step_run_id)
                    && (step.status == runbook_model::StepStatus::Claimed
                        || step
                            .approval
                            .as_ref()
                            .and_then(|a| a.decision_lease.as_ref())
                            .is_some_and(|lease| lease.lease_id == claim.claim_id))
            })
        })
        .map(|claim| crate::ClaimWrite {
            action: crate::ClaimAction::Release,
            expected_generation: claim.generation,
            released: true,
            ..claim.clone()
        })
        .collect()
}
