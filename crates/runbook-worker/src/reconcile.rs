//! Review fix 2: starts whose D1 half committed but whose run never reached
//! D1. The minute Cron lists them (`unsettled_starts`, past
//! `START_GRACE_MS`) and asks each run's object to [`one`] it: an object
//! that initialized has its derived rows drained (finished); an empty one
//! is abandoned in D1 (admission released, replay key freed, run
//! tombstoned as `start.abandoned`). A retry of the start inside the grace
//! period finishes it instead (`CfStore::commit_start`).
use crate::{
    compose::{CfStore, Objects},
    object::ObjectHandle,
};
use runbook_core::StoreError;
use runbook_model::ExecutionId;
use runbook_store_d1::D1;

use runbook_store_r2::ObjectBucket;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub finished: Vec<ExecutionId>,
    pub abandoned: Vec<ExecutionId>,
}

/// What [`one`] did to a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Finished,
    Abandoned,
    /// It reached D1 meanwhile, or another pass handled it.
    Untouched,
}

/// Reconcile one unsettled start on a host that reaches its object.
pub async fn one<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    id: &ExecutionId,
) -> Result<Outcome, StoreError> {
    let object = store.objects().object(id, store.scope())?;
    match object.load(id).await {
        Ok(_) => {
            store.drain_derived(id).await?;
            Ok(Outcome::Finished)
        }
        Err(StoreError::NotFound) => Ok(if store.catalog().abandon_start(id).await? {
            // CF6: an abandoned start never ran: its count goes back.
            use crate::quota::QuotaHandle;
            let _ = store.org_quota().refund(id).await;
            Outcome::Abandoned
        } else {
            Outcome::Untouched
        }),
        Err(error) => Err(error),
    }
}

/// One pass over unsettled starts older than `grace_ms`, for a host that
/// reaches every object (tests; the Cron sends each to its object).
pub async fn pass<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    now_ms: i64,
    grace_ms: i64,
) -> Result<Reconciled, StoreError> {
    let mut out = Reconciled::default();
    let due =
        runbook_store_d1::start::unsettled_starts(store.catalog().db(), now_ms - grace_ms, 50)
            .await?;
    for (scope, id) in due {
        match one(&store.scoped(scope), &id).await? {
            Outcome::Finished => out.finished.push(id),
            Outcome::Abandoned => out.abandoned.push(id),
            Outcome::Untouched => {}
        }
    }
    Ok(out)
}
