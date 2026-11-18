//! B4.4: process one claimed work lease.
//!
//! - Activation leases start a run (D24) through `start::activate`.
//! - Timer leases open the run's session, feed due step timers and
//!   time-driven transitions to the engine at the store's post-lock time,
//!   settle through the shared runtime, commit, then acknowledge.
//! - Claim-lease leases expire an agent claim whose deadline passed (B6a),
//!   returning the step to ready through `claims::expire_claim`.
//! - Notification leases belong to webhook delivery (C1): `process_lease`
//!   leaves them alone and `process_notification` fans each committed
//!   outbox intent out to matching subscriptions, then acknowledges it.
//!
//! Exactly-once transitions come from the store, not this function: a commit
//! replaces the fired timer's row, so a lease still held after a crash between
//! commit and acknowledgement no longer matches the stored timer and is
//! treated as already applied. No network call or blob upload happens while
//! the execution session is open.
use crate::{
    CommitResult, StoreError,
    actor::{ActorError, begin_actor},
    blob::BlobStore,
    catalog::{CatalogStore, TenantScope},
    directory::DirectoryStore,
    session::SessionStore,
    start::{StartStore, activate},
    work_queue::{WorkKind, WorkLease, WorkQueue},
};
use runbook_model::ActorRef;

/// What processing one lease did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Processed {
    /// An activation started the run.
    Activated,
    /// An expired claim lease returned its step to ready.
    ClaimExpired,
    /// The timer fired and its transitions were committed and acknowledged.
    Fired,
    /// The lease no longer matches stored state (already applied or replaced);
    /// it was acknowledged if the queue still knew it.
    Obsolete,
    /// The timer was current but nothing was due yet; left for a later claim.
    NotDue,
    /// Another writer committed first; the lease is left to expire and retry.
    Retry,
    /// Not this worker's kind of work (webhook notifications).
    Skipped,
    /// A notification was fanned out to its matching subscriptions (C1).
    Notified,
}

/// Bound on time-driven rounds in one commit; a correct engine needs a few.
const MAX_ROUNDS: usize = 1000;

pub async fn process_lease<S, D>(
    store: &S,
    directory: &D,
    scope: TenantScope,
    lease: &WorkLease,
) -> Result<Processed, ActorError>
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore,
    D: DirectoryStore,
{
    match &lease.id.kind {
        WorkKind::Activation => {
            activate(store, directory, scope, lease).await?;
            Ok(Processed::Activated)
        }
        WorkKind::Notification { .. } => Ok(Processed::Skipped),
        WorkKind::ClaimLease {
            step_run_id,
            generation,
        } => {
            let work = begin_actor(store, directory, scope, &lease.id.execution_id).await?;
            // `false` covers both a claim that is no longer current and a lost
            // commit race, so only a committed expiry is acknowledged. Work that
            // is no longer current stops being offered; a lost race retries.
            if crate::claims::expire_claim(work, step_run_id, *generation).await? {
                acknowledge(store, lease).await?;
                Ok(Processed::ClaimExpired)
            } else {
                Ok(Processed::Retry)
            }
        }
        // LANG4: a call's child start, cancellation or result.
        WorkKind::Timer { key, generation } if crate::calls::is_call_key(key) => {
            crate::calls::process(store, directory, scope, lease, key, *generation).await
        }
        WorkKind::Timer { key, generation } => {
            fire_timer(store, directory, scope, lease, key, *generation).await
        }
    }
}

async fn fire_timer<S, D>(
    store: &S,
    directory: &D,
    scope: TenantScope,
    lease: &WorkLease,
    key: &str,
    generation: u64,
) -> Result<Processed, ActorError>
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore,
    D: DirectoryStore,
{
    let mut work = begin_actor(store, directory, scope, &lease.id.execution_id).await?;
    let current = work.snapshot.timers.get(key).map(|timer| timer.generation);
    if current != Some(generation) {
        drop(work);
        acknowledge(store, lease).await?;
        return Ok(Processed::Obsolete);
    }
    let at = work.at_ms();
    for _ in 0..MAX_ROUNDS {
        let before = work.runtime.pending_events().len();
        for event in runbook_engine::due_timers(&work.runtime.state, at)? {
            work.append(event, ActorRef::System)?;
        }
        for command in runbook_engine::advance_time(&work.runtime.state, at)? {
            work.runtime.realize(command)?;
        }
        work.settle()?;
        if work.runtime.pending_events().len() == before {
            break;
        }
    }
    if !work.has_pending() {
        // Current but not yet due: the session closes without writing, and
        // the lease expires so a later claim retries at the due time.
        return Ok(Processed::NotDue);
    }
    match work.commit(vec![]).await? {
        CommitResult::Committed(_) => {
            acknowledge(store, lease).await?;
            Ok(Processed::Fired)
        }
        CommitResult::Conflict => Ok(Processed::Retry),
    }
}

/// Acknowledge a lease, treating work the queue no longer offers as done.
async fn acknowledge<S: WorkQueue>(store: &S, lease: &WorkLease) -> Result<(), StoreError> {
    match store.ack(&lease.id, lease.generation).await {
        Ok(()) | Err(StoreError::InvalidBatch("queue.work_obsolete")) => Ok(()),
        Err(error) => Err(error),
    }
}

/// C1 fan-out of one notification lease: load the committed intent, record
/// its envelope and one delivery per matching enabled subscription
/// (idempotent on the event ID), then acknowledge. A crash before the
/// acknowledgement re-runs this harmlessly; the event keeps its ID.
pub async fn process_notification<S, W>(
    store: &S,
    webhooks: &W,
    scope: TenantScope,
    lease: &WorkLease,
) -> Result<Processed, StoreError>
where
    S: StartStore + CatalogStore + WorkQueue,
    W: crate::webhooks::WebhookStore,
{
    let WorkKind::Notification { event_id } = &lease.id.kind else {
        return Ok(Processed::Skipped);
    };
    let id = &lease.id.execution_id;
    let run = store.load(id).await?;
    let Some(intent) = run.outbox.get(event_id) else {
        acknowledge(store, lease).await?;
        return Ok(Processed::Obsolete);
    };
    let summary = store.get_run(id).await?.ok_or(StoreError::NotFound)?;
    let record = store.start_record(id).await?.ok_or(StoreError::NotFound)?;
    let meta = crate::webhooks::ExecutionMeta {
        id: id.clone(),
        title: summary.title.clone(),
        runbook_id: record.runbook_id.clone(),
        version_id: record.version_id.clone(),
        version: record.version.map(std::num::NonZeroU32::get),
        url: intent.link.clone(),
        tags: summary.tags.clone(),
    };
    let subscriptions = webhooks.list_subscriptions(&scope).await?;
    let (event, deliveries) = crate::webhooks::plan_fan_out(&scope, intent, &meta, &subscriptions)?;
    let recorded = webhooks
        .record_event(&scope, event, deliveries, None)
        .await?;
    match acknowledge(store, lease).await {
        Ok(()) => Ok(Processed::Notified),
        // Another worker already fanned this event out and acknowledged it.
        Err(StoreError::InvalidBatch("queue.stale_lease")) if !recorded => Ok(Processed::Obsolete),
        Err(error) => Err(error),
    }
}
