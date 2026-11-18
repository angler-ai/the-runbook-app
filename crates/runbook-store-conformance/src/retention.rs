use crate::*;
use runbook_core::{
    blob::{BlobKey, BlobPrefix, BlobStore},
    catalog::{AdmissionState, AdmitRequest, CatalogStore},
    retention::RunStore,
    work_queue::WorkQueue,
};
use runbook_model::ExecutionStatus;
pub(crate) async fn seed<F: QueueFactory>(f: &F) -> Result<F::Store, StoreError> {
    let s = f.create(execution::projection()).await?;
    for n in [1, 2] {
        s.admit(AdmitRequest {
            execution_id: catalog::id(n),
            key: "api".into(),
            limit: 1,
            idempotency_key: format!("start-{n}"),
        })
        .await?;
    }
    s.commit(execution::batch()).await?;
    s.claim_due(1, 60_000).await?;
    s.commit(catalog::batch(1, 1, 2, ExecutionStatus::Succeeded))
        .await?;
    s.commit(catalog::batch(2, 0, 1, ExecutionStatus::Succeeded))
        .await?;
    for n in [1, 2] {
        s.put(
            BlobKey::Execution {
                execution_id: catalog::id(n),
                name: "evidence".into(),
            },
            &[n],
        )
        .await?;
    }
    s.put(BlobKey::Shared { hash: [7; 32] }, b"snapshot")
        .await?;
    Ok(s)
}
pub async fn export_contains_one_complete_run<F: QueueFactory>(f: &F) -> Result<(), StoreError> {
    let s = seed(f).await?;
    let slice = s.export_execution(&catalog::id(1)).await?;
    assert_eq!(slice.run, s.load(&catalog::id(1)).await?);
    assert_eq!(slice.run.events.len(), 2);
    assert_eq!(
        slice.catalog.admission.as_ref().unwrap().0.idempotency_key,
        "start-1"
    );
    assert_eq!(
        slice.catalog.admission.as_ref().unwrap().1.state,
        AdmissionState::Released
    );
    assert_eq!(slice.run.claims.len(), 1);
    assert_eq!(slice.run.idempotency.len(), 1);
    assert_eq!(slice.run.timer_history.len(), 1);
    assert_eq!(slice.run.outbox.len(), 1);
    assert_eq!(slice.run.derived_outbox.len(), 1);
    assert_eq!(slice.work.len(), 1);
    assert_eq!(slice.catalog.summary, s.get_run(&catalog::id(1)).await?);
    assert_eq!(
        slice.blob_keys,
        vec![BlobKey::Execution {
            execution_id: catalog::id(1),
            name: "evidence".into()
        }]
    );
    assert_eq!(s.get(&slice.blob_keys[0]).await?, vec![1]);
    assert_eq!(slice.scope, scope(1));
    let q = f.scoped_client(&s, scope(2)).await?;
    assert_eq!(
        q.export_execution(&catalog::id(1)).await,
        Err(StoreError::NotFound)
    );
    Ok(())
}
pub async fn delete_leaves_only_tombstone<F: QueueFactory>(f: &F) -> Result<(), StoreError> {
    let s = seed(f).await?;
    let other = s.export_execution(&catalog::id(2)).await?;
    let before_counts = f.owned_record_counts(&s, &catalog::id(1)).await?;
    assert!(before_counts["blob"] > 0 && before_counts["work_lease"] > 0);
    let sibling_counts = f.owned_record_counts(&s, &catalog::id(2)).await?;
    let first = s.delete_execution(&catalog::id(1), "retention").await?;
    let after_counts = f.owned_record_counts(&s, &catalog::id(1)).await?;
    assert_eq!(
        after_counts.keys().collect::<Vec<_>>(),
        before_counts.keys().collect::<Vec<_>>()
    );
    for (table, count) in after_counts {
        assert_eq!(
            count,
            usize::from(table == "tombstone"),
            "orphaned rows in {table}"
        );
    }
    assert_eq!(
        f.owned_record_counts(&s, &catalog::id(2)).await?,
        sibling_counts
    );
    assert_eq!(first.execution_id, catalog::id(1));
    assert_eq!(first.reason, "retention");
    assert_eq!(s.tombstone(&catalog::id(1)).await?, Some(first.clone()));
    assert_eq!(s.load(&catalog::id(1)).await, Err(StoreError::NotFound));
    assert_eq!(
        s.export_execution(&catalog::id(1)).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(s.get_run(&catalog::id(1)).await?, None);
    assert_eq!(s.admission(&catalog::id(1)).await?, None);
    assert!(
        s.list_assignments()
            .await?
            .iter()
            .all(|a| a.execution_id != catalog::id(1))
    );
    assert!(
        s.claim_due(100, 60_000)
            .await?
            .iter()
            .all(|w| w.id.execution_id != catalog::id(1))
    );
    assert_eq!(
        s.get(&BlobKey::Execution {
            execution_id: catalog::id(1),
            name: "evidence".into()
        })
        .await,
        Err(StoreError::NotFound)
    );
    assert_eq!(
        s.get(&BlobKey::Shared { hash: [7; 32] }).await?,
        b"snapshot"
    );
    assert_eq!(s.export_execution(&catalog::id(2)).await?, other);
    assert!(
        s.commit(catalog::batch(1, 0, 1, ExecutionStatus::Running))
            .await
            .is_err()
    );
    assert!(
        s.admit(AdmitRequest {
            execution_id: catalog::id(1),
            key: "api".into(),
            limit: 1,
            idempotency_key: "start-1".into()
        })
        .await
        .is_err()
    );
    assert_eq!(
        s.admit(AdmitRequest {
            execution_id: catalog::id(3),
            key: "api".into(),
            limit: 1,
            idempotency_key: "start-1".into()
        })
        .await?
        .state,
        AdmissionState::Admitted
    );
    let q = f.scoped_client(&s, scope(2)).await?;
    assert_eq!(q.tombstone(&catalog::id(1)).await?, None);
    assert_eq!(
        q.delete_execution(&catalog::id(2), "foreign").await,
        Err(StoreError::NotFound)
    );
    assert_eq!(s.delete_execution(&catalog::id(1), "retry").await?, first);
    Ok(())
}
pub async fn finished_day_uses_utc_half_open_interval<F: QueueFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    for (n, time) in [(1, 0), (2, 86_399_999), (3, 86_400_000), (4, 0)] {
        let mut b = catalog::batch(n, 0, 1, ExecutionStatus::Succeeded);
        b.projection.completed_at_ms = Some(time);
        b.events[0].at_ms = time;
        b.derived_outbox[0].summary.completed_at_ms = Some(time);
        if n == 4 {
            b.projection.runbook_id = "rb_00000000000000000000000009".parse().unwrap();
            b.derived_outbox[0].summary.runbook_id = b.projection.runbook_id.clone();
        }
        s.commit(b).await?;
    }
    let rb = execution::projection().runbook_id;
    assert_eq!(
        s.finished_on(&rb, 0).await?,
        vec![catalog::id(1), catalog::id(2)]
    );
    assert_eq!(s.finished_on(&rb, 1).await?, vec![catalog::id(3)]);
    let q = f.scoped_client(&s, scope(2)).await?;
    assert!(q.finished_on(&rb, 0).await?.is_empty());
    assert!(s.list_prefix(&BlobPrefix::Shared).await?.is_empty());
    Ok(())
}

pub async fn deletion_is_atomic_and_rejects_open_runs<F: QueueFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = seed(f).await?;
    let before = s.export_execution(&catalog::id(1)).await?;
    f.fail_after(&s, WriteStage::RunDeletion).await?;
    assert!(s.delete_execution(&catalog::id(1), "fault").await.is_err());
    assert_eq!(s.export_execution(&catalog::id(1)).await?, before);
    assert_eq!(s.tombstone(&catalog::id(1)).await?, None);
    assert_eq!(s.get(&before.blob_keys[0]).await?, vec![1]);
    s.commit(catalog::batch(3, 0, 1, ExecutionStatus::Running))
        .await?;
    assert!(s.delete_execution(&catalog::id(3), "open").await.is_err());
    assert!(s.export_execution(&catalog::id(3)).await.is_err());
    assert_eq!(
        s.load(&catalog::id(3)).await?.projection.status,
        ExecutionStatus::Running
    );
    Ok(())
}
