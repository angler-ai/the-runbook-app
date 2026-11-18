use crate::*;
use runbook_core::work_queue::*;
async fn seed<F: WorkQueueFactory>(f: &F) -> Result<F::Store, StoreError> {
    let s = f.create(execution::projection()).await?;
    let now = f.backend_time_ms(&s).await?;
    let mut b = execution::batch();
    b.outbox.clear();
    b.claims.clear();
    b.idempotency.clear();
    let TimerChange::Upsert(timer) = &mut b.timers[0] else {
        unreachable!()
    };
    timer.due_at_ms = now;
    let mut later = timer.clone();
    later.key = "later".into();
    later.due_at_ms = now + 86_400_000;
    b.timers.push(TimerChange::Upsert(later));
    s.commit(b).await?;
    Ok(s)
}
pub async fn due_item_has_one_current_worker<F: WorkQueueFactory>(f: &F) -> Result<(), StoreError> {
    let s = seed(f).await?;
    let other = s.clone();
    let (a, b) = execution::together(s.claim_due(1, 60_000), other.claim_due(1, 60_000)).await;
    let mut leases = a?;
    leases.extend(b?);
    assert_eq!(leases.len(), 1);
    assert!(matches!(&leases[0].id.kind,WorkKind::Timer{key,..}if key=="deadline"));
    s.ack(&leases[0].id, leases[0].generation).await?;
    assert!(s.claim_due(10, 60_000).await?.is_empty());
    Ok(())
}
pub async fn stale_worker_cannot_ack_successor<F: WorkQueueFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = seed(f).await?;
    let first = s.claim_due(1, 5).await?.remove(0);
    f.expire_lease(&s, &first).await?;
    assert!(s.ack(&first.id, first.generation).await.is_err());
    let second = s.claim_due(1, 60_000).await?.remove(0);
    assert_eq!(first.id, second.id);
    assert_eq!(second.generation, first.generation + 1);
    assert!(s.ack(&first.id, first.generation).await.is_err());
    s.ack(&second.id, second.generation).await?;
    assert!(s.claim_due(1, 60_000).await?.is_empty());
    Ok(())
}
pub async fn delivery_leases_are_scoped_and_fenced<F: WorkQueueFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    let q = f.scoped_client(&s, scope(2)).await?;
    let mut batch = execution::batch();
    batch.timers.clear();
    s.commit(batch).await?;
    assert!(q.claim_due(10, 60_000).await?.is_empty());
    let lease = s.claim_due(1, 60_000).await?.remove(0);
    assert!(matches!(lease.id.kind, WorkKind::Notification { .. }));
    assert_eq!(lease.scope, scope(1));
    assert_eq!(
        q.ack(&lease.id, lease.generation).await,
        Err(StoreError::NotFound)
    );
    assert!(s.ack(&lease.id, lease.generation + 1).await.is_err());
    s.ack(&lease.id, lease.generation).await?;
    assert!(s.ack(&lease.id, lease.generation).await.is_err());
    assert!(s.claim_due(10, 60_000).await?.is_empty());
    Ok(())
}
pub async fn timer_replacements_require_new_generation<F: WorkQueueFactory>(
    f: &F,
) -> Result<(), StoreError> {
    for acknowledged in [false, true] {
        let s = seed(f).await?;
        let lease = s.claim_due(1, 60_000).await?.remove(0);
        if acknowledged {
            s.ack(&lease.id, lease.generation).await?
        }
        let before = s.load(&execution::projection().execution_id).await?;
        let old = before.timers["deadline"].clone();
        let mut replacement = old.clone();
        replacement.due_at_ms -= 1;
        let mut b = execution::batch();
        b.expected_sequence = 1;
        b.projection.sequence = 2;
        b.events[0].seq = 2;
        b.outbox.clear();
        b.idempotency.clear();
        b.claims.clear();
        b.timers = vec![TimerChange::Upsert(replacement.clone())];
        assert!(s.commit(b.clone()).await.is_err());
        assert_eq!(s.load(&old.execution_id).await?, before);
        b.timers = vec![TimerChange::Delete {
            execution_id: old.execution_id.clone(),
            key: old.key.clone(),
        }];
        s.commit(b.clone()).await?;
        b.expected_sequence = 2;
        b.projection.sequence = 3;
        b.events[0].seq = 3;
        b.timers = vec![TimerChange::Upsert(old.clone())];
        assert!(s.commit(b.clone()).await.is_err());
        replacement.generation += 1;
        b.timers = vec![TimerChange::Upsert(replacement)];
        s.commit(b).await?;
        assert!(s.ack(&lease.id, lease.generation).await.is_err());
        let new = s.claim_due(1, 60_000).await?.remove(0);
        assert!(matches!(new.id.kind, WorkKind::Timer { generation: 2, .. }));
        s.ack(&new.id, new.generation).await?;
    }
    Ok(())
}
