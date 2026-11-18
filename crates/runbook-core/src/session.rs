//! Transaction-scoped time and snapshot. Evaluate purely while a session is open;
//! never make external calls or upload blobs while holding a backend row lock.
use crate::{ExecutionStore, StoreError};
use runbook_model::ExecutionId;
use std::future::Future;
pub trait ExecutionSession: ExecutionStore {
    fn transaction_time_ms(&self) -> i64;
    /// Lookup only this locked execution's actor replay prefix, without loading history.
    fn actor_replay(
        &self,
        prefix: &str,
    ) -> impl Future<Output = Result<Option<crate::IdempotencyRecord>, StoreError>>;
    /// End the session's transaction without writing and release its locks
    /// and connection now; later commits on it fail as closed. LANG3 reads
    /// a holder this way and prepares a commit that the start transaction
    /// applies (fenced by its sequence), so no lock or connection is held
    /// while it prepares.
    fn release(&self) -> impl Future<Output = ()> {
        async {}
    }
}
pub trait SessionStore: ExecutionStore {
    type Session: ExecutionSession;
    fn begin(
        &self,
        execution_id: &ExecutionId,
    ) -> impl Future<Output = Result<Self::Session, StoreError>>;
    /// CF13 review round 1: the most a run's event log may hold before a
    /// request's data-carrying write is refused ([`crate::run_budget`]).
    fn run_budget(&self) -> u64 {
        crate::run_budget::DEFAULT_MAX_RUN_BYTES
    }
}
