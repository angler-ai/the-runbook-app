//! Store-clock leases for timers and notification delivery. Generation fences acknowledgements.
use crate::{StoreError, catalog::TenantScope};
use runbook_model::ExecutionId;
use serde::{Deserialize, Serialize};
use std::future::Future;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum WorkKind {
    Timer {
        key: String,
        generation: u64,
    },
    Notification {
        event_id: String,
    },
    /// Durable start activation for an admitted, unstarted run (D24).
    Activation,
    /// A claim lease deadline for one claimed step run (B6a).
    ClaimLease {
        step_run_id: runbook_model::StepRunId,
        generation: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WorkId {
    pub execution_id: ExecutionId,
    pub kind: WorkKind,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkLease {
    pub scope: TenantScope,
    pub id: WorkId,
    pub generation: u64,
    pub deadline_at_ms: i64,
}
pub trait WorkQueue {
    fn claim_due(
        &self,
        limit: usize,
        lease_ms: u64,
    ) -> impl Future<Output = Result<Vec<WorkLease>, StoreError>>;
    fn ack(&self, id: &WorkId, generation: u64) -> impl Future<Output = Result<(), StoreError>>;
}
#[derive(Clone)]
pub(crate) struct LeaseRecord {
    pub lease: WorkLease,
    pub acknowledged: bool,
}
