//! Run-owned storage primitives, not a retention scheduler or public archive API.
use crate::{
    StoreError, StoredExecution,
    blob::BlobKey,
    catalog::{RunCatalogSlice, TenantScope},
    work_queue::WorkLease,
};
use runbook_model::{ExecutionId, RunbookId};
use serde::{Deserialize, Serialize};
use std::future::Future;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionSlice {
    pub scope: TenantScope,
    pub run: StoredExecution,
    #[serde(default)]
    pub actor_replays: Vec<crate::IdempotencyRecord>,
    pub catalog: RunCatalogSlice,
    pub work: Vec<WorkState>,
    pub blob_keys: Vec<BlobKey>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkState {
    pub lease: WorkLease,
    pub acknowledged: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub scope: TenantScope,
    pub execution_id: ExecutionId,
    pub runbook_id: RunbookId,
    pub deleted_at_ms: i64,
    pub reason: String,
}
pub trait RunStore {
    fn export_execution(
        &self,
        id: &ExecutionId,
    ) -> impl Future<Output = Result<ExecutionSlice, StoreError>>;
    fn delete_execution(
        &self,
        id: &ExecutionId,
        reason: &str,
    ) -> impl Future<Output = Result<Tombstone, StoreError>>;
    fn tombstone(
        &self,
        id: &ExecutionId,
    ) -> impl Future<Output = Result<Option<Tombstone>, StoreError>>;
    /// UTC day number since Unix epoch; interval [day*86400000,(day+1)*86400000).
    fn finished_on(
        &self,
        runbook: &RunbookId,
        utc_day: i64,
    ) -> impl Future<Output = Result<Vec<ExecutionId>, StoreError>>;
}
