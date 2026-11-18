//! What an ExecutionDO's host does after every request and on every alarm
//! (CF3, replacing CF1's spike alarm): due work through core's
//! `drain::process_lease` (activation, timers, claim leases; notification
//! intents leave through the queue publisher, CF4), then the run's derived
//! rows into D1, then admission notices for waiters this object can reach.
use crate::{
    cloudflare::{CfBackends, CfHost},
    compose::Objects,
    object::ObjectHandle,
};
use runbook_api::backends::Backends;
use runbook_core::catalog::TenantScope;
use runbook_core::{
    StoreError,
    drain::{Processed, process_lease},
};
use runbook_model::ExecutionId;

/// Leases one settle takes at most, and how long a lease holds.
pub const SETTLE_BATCH: usize = 16;
pub const SETTLE_LEASE_MS: u64 = 60_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settled {
    pub processed: Vec<Processed>,
    pub derived: usize,
    pub promoted: Vec<runbook_store_d1::admission::Promotion>,
}

/// Settle `id`'s object for `scope`, the tenant its own head names (the
/// alarm reads it there; a request has it from its verified credential).
/// An object with no run yet has nothing to settle.
pub async fn settle<H: CfHost>(
    backends: &CfBackends<H>,
    id: &ExecutionId,
    scope: &TenantScope,
) -> Result<Settled, StoreError> {
    let scope = scope.clone();
    let store = backends.execution(scope.clone());
    let object = backends.host().objects().object(id, &scope)?;
    if object.current_execution().await?.is_none() {
        return Ok(Settled::default());
    }
    let mut settled = Settled::default();
    let leases = object
        .claim_settle_work(SETTLE_BATCH, SETTLE_LEASE_MS)
        .await?;
    let directory = backends.directory();
    for lease in &leases {
        match process_lease(&store, &directory, scope.clone(), lease).await {
            Ok(done) => settled.processed.push(done),
            // The lease stays; it is offered again when it lapses.
            Err(_) => settled.processed.push(Processed::Retry),
        }
    }
    let drained = store.drain_derived(id).await?;
    settled.derived = drained.applied;
    store.deliver_notices(&drained.promoted).await?;
    settled.promoted = drained.promoted;
    Ok(settled)
}
