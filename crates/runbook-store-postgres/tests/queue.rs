//! B2.3: B2a.3 queue cases unchanged on Postgres, plus the two-worker
//! generation fence across independent pools and a restart.
mod support;
use runbook_core::{
    ExecutionStore, StoreError, TimerChange,
    work_queue::{WorkKind, WorkQueue},
};
use runbook_store_conformance::{SessionFactory, WorkQueueFactory, scope};
use support::factory::{PostgresFactory, with_factory};

macro_rules! queue_cases {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| runbook_store_conformance::queue::$name(f).await).await;
        }
    )*};
}
queue_cases!(
    due_item_has_one_current_worker,
    stale_worker_cannot_ack_successor,
    delivery_leases_are_scoped_and_fenced,
    timer_replacements_require_new_generation,
);

/// Mirror of the shared queue seed: one due timer, one not due.
async fn seed(
    f: &PostgresFactory,
) -> Result<runbook_store_postgres::PostgresExecutionStore, StoreError> {
    use runbook_store_conformance::StoreFactory;
    let s = f
        .create(runbook_store_conformance::execution::projection())
        .await?;
    let now = f.backend_time_ms(&s).await?;
    let mut b = runbook_store_conformance::execution::batch();
    b.outbox.clear();
    b.claims.clear();
    b.idempotency.clear();
    let TimerChange::Upsert(timer) = &mut b.timers[0] else {
        unreachable!()
    };
    timer.due_at_ms = now;
    s.commit(b).await?;
    Ok(s)
}

/// Two workers on independent pools cannot both complete the same timer
/// generation: the expired worker's ack is fenced, the successor's succeeds
/// exactly once, and a restarted store does not see the work again.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn two_processes_cannot_complete_same_generation() {
    with_factory(async |f| {
        let seeded = seed(f).await?;
        let worker_a = f.fresh_store(scope(1)).await;
        let worker_b = f.fresh_store(scope(1)).await;
        let (a, b) = tokio::join!(worker_a.claim_due(1, 60_000), worker_b.claim_due(1, 60_000));
        let mut leases = a?;
        leases.extend(b?);
        assert_eq!(leases.len(), 1);
        let first = leases.remove(0);
        assert!(matches!(first.id.kind, WorkKind::Timer { .. }));

        f.expire_lease(&seeded, &first).await?;
        let second = worker_b.claim_due(1, 60_000).await?.remove(0);
        assert_eq!(second.id, first.id);
        assert_eq!(second.generation, first.generation + 1);
        assert!(worker_a.ack(&first.id, first.generation).await.is_err());
        worker_b.ack(&second.id, second.generation).await?;
        assert!(worker_b.ack(&second.id, second.generation).await.is_err());

        drop(worker_a);
        drop(worker_b);
        drop(seeded);
        let restarted = f.fresh_store(scope(1)).await;
        assert!(restarted.claim_due(10, 60_000).await?.is_empty());
        Ok(())
    })
    .await;
}

/// HOST1: a stopping worker gives back a lease it will not process; another
/// worker claims the item at once (no wait for the 60 s lease), and the
/// release is fenced: a stale or acknowledged lease is not released.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_released_lease_is_claimable_at_once_and_fenced() {
    with_factory(async |f| {
        let _seeded = seed(f).await?;
        let stopping = f.fresh_store(scope(1)).await;
        let other = f.fresh_store(scope(1)).await;
        let first = stopping.claim_due(1, 60_000).await?.remove(0);
        assert!(other.claim_due(1, 60_000).await?.is_empty(), "leased");
        assert!(stopping.release_lease(&first).await?);
        let second = other.claim_due(1, 60_000).await?.remove(0);
        assert_eq!(second.id, first.id);
        assert_eq!(second.generation, first.generation + 1);
        // The old holder can neither release nor ack its successor's lease.
        assert!(!stopping.release_lease(&first).await?);
        assert!(stopping.ack(&first.id, first.generation).await.is_err());
        other.ack(&second.id, second.generation).await?;
        assert!(!other.release_lease(&second).await?, "acknowledged");
        Ok(())
    })
    .await;
}
