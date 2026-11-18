use crate::*;
use runbook_core::catalog::*;
use runbook_engine::{Event, EventEnvelope};
use runbook_model::ActorRef;
use runbook_model::{ExecutionId, ExecutionStatus, RecipientRef};
pub fn id(n: u8) -> ExecutionId {
    format!("exe_{n:026}").parse().unwrap()
}
pub fn update(n: u8, seq: u64, status: ExecutionStatus) -> DerivedUpdate {
    let terminal = matches!(
        status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    );
    DerivedUpdate {
        summary: RunSummary {
            execution_id: id(n),
            runbook_id: execution::projection().runbook_id,
            sequence: seq,
            status,
            completed_at_ms: terminal.then_some(10),
            title: format!("Verify run {n}"),
            tags: vec!["deploy".into()],
            fields: Default::default(),
            created_by: "operator".into(),
            created_at_ms: 1_000 + i64::from(n),
        },
        assignments: if terminal {
            vec![]
        } else {
            vec![Assignment {
                execution_id: id(n),
                step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
                recipients: vec![RecipientRef::User(
                    "usr_00000000000000000000000001".parse().unwrap(),
                )],
                kind: runbook_core::catalog::AssignmentKind::Work,
                sequence: seq,
                updated_at_ms: 100,
                voted_by: vec![],
                path: String::new(),
                title: String::new(),
            }]
        },
        release_concurrency: terminal,
        memory: vec![],
    }
}
pub fn batch(n: u8, from: u64, to: u64, status: ExecutionStatus) -> ExecutionCommit {
    let mut p = execution::projection();
    p.execution_id = id(n);
    p.sequence = to;
    p.status = status;
    p.completed_at_ms = update(n, to, status).summary.completed_at_ms;
    ExecutionCommit {
        execution_id: id(n),
        expected_sequence: from,
        events: (from + 1..=to)
            .map(|seq| EventEnvelope {
                execution_id: id(n),
                seq,
                at_ms: 10,
                actor: ActorRef::System,
                event: Event::ManualPauseChanged { paused: false },
            })
            .collect(),
        projection: p,
        timers: vec![],
        outbox: vec![],
        idempotency: vec![],
        claims: vec![],
        credentials: vec![],
        derived_outbox: vec![update(n, to, status)],
    }
}
async fn settled<F: CatalogFactory>(
    f: &F,
    s: &F::Store,
    n: u8,
    seq: u64,
) -> Result<(), StoreError> {
    f.await_derived(s, &id(n), seq, f.derived_row_delay()).await
}
pub async fn project_queries_never_return_foreign_rows<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let p = f.create(execution::projection()).await?;
    let q = f.scoped_client(&p, scope(2)).await?;
    p.commit(batch(1, 0, 1, ExecutionStatus::Running)).await?;
    q.commit(batch(2, 0, 1, ExecutionStatus::Running)).await?;
    settled(f, &p, 1, 1).await?;
    settled(f, &q, 2, 1).await?;
    assert_eq!(
        p.list_runs().await?,
        vec![update(1, 1, ExecutionStatus::Running).summary]
    );
    assert_eq!(
        q.list_runs().await?,
        vec![update(2, 1, ExecutionStatus::Running).summary]
    );
    assert_eq!(p.get_run(&id(2)).await?, None);
    assert_eq!(p.load(&id(2)).await, Err(StoreError::NotFound));
    assert_eq!(q.load(&id(1)).await, Err(StoreError::NotFound));
    assert!(
        p.list_assignments()
            .await?
            .iter()
            .all(|a| a.execution_id == id(1))
    );
    assert!(
        q.commit(batch(1, 1, 2, ExecutionStatus::Running))
            .await
            .is_err()
    );
    Ok(())
}
pub async fn revision_precondition_rejects_stale_replace<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let p = f.create(execution::projection()).await?;
    let q = f.scoped_client(&p, scope(2)).await?;
    assert_eq!(
        p.replace_draft("draft", 0, "one".into()).await?,
        CommitResult::Committed(1)
    );
    assert_eq!(
        p.replace_draft("draft", 1, "two".into()).await?,
        CommitResult::Committed(2)
    );
    assert_eq!(
        p.replace_draft("draft", 1, "stale".into()).await?,
        CommitResult::Conflict
    );
    assert_eq!(p.read_draft("draft").await?.unwrap().source, "two");
    assert_eq!(q.read_draft("draft").await?, None);
    assert_eq!(
        q.replace_draft("draft", 0, "foreign".into()).await?,
        CommitResult::Committed(1)
    );
    assert_eq!(p.read_draft("draft").await?.unwrap().revision, 2);
    Ok(())
}
pub async fn derived_rows_ignore_old_or_duplicate_sequence<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    s.commit(batch(1, 0, 4, ExecutionStatus::Running)).await?;
    s.commit(batch(1, 4, 5, ExecutionStatus::Succeeded)).await?;
    settled(f, &s, 1, 5).await?;
    f.redeliver_derived(&s, &id(1), 5).await?;
    f.redeliver_derived(&s, &id(1), 4).await?;
    settled(f, &s, 1, 5).await?;
    assert_eq!(
        s.get_run(&id(1)).await?,
        Some(update(1, 5, ExecutionStatus::Succeeded).summary)
    );
    assert!(s.list_assignments().await?.is_empty());
    Ok(())
}
fn request(n: u8, key: &str) -> AdmitRequest {
    AdmitRequest {
        execution_id: id(n),
        key: key.into(),
        limit: 1,
        idempotency_key: format!("start-{n}"),
    }
}
pub async fn concurrency_limit_one_admits_and_queues<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    let other = s.clone();
    let (a, b) =
        execution::together(s.admit(request(1, "api")), other.admit(request(2, "api"))).await;
    let (a, b) = (a?, b?);
    assert_eq!(
        [&a, &b]
            .iter()
            .filter(|r| r.state == AdmissionState::Admitted)
            .count(),
        1
    );
    assert_eq!(
        [&a, &b]
            .iter()
            .filter(|r| r.state == AdmissionState::Queued)
            .count(),
        1
    );
    let (first, next) = if a.state == AdmissionState::Admitted {
        (1, 2)
    } else {
        (2, 1)
    };
    assert_eq!(
        s.admit(request(3, "api")).await?.state,
        AdmissionState::Queued
    );
    assert_eq!(
        s.admit(request(4, "other")).await?.state,
        AdmissionState::Admitted
    );
    s.commit(batch(first, 0, 1, ExecutionStatus::Succeeded))
        .await?;
    settled(f, &s, first, 1).await?;
    f.redeliver_derived(&s, &id(first), 1).await?;
    assert_eq!(
        s.admission(&id(next)).await?.unwrap().state,
        AdmissionState::Admitted
    );
    assert_eq!(
        s.admission(&id(3)).await?.unwrap().state,
        AdmissionState::Queued
    );
    assert_eq!(
        s.admit(request(first, "api")).await?.state,
        AdmissionState::Released
    );
    Ok(())
}
pub async fn derived_rows_rollback_with_execution<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    s.admit(request(1, "api")).await?;
    s.admit(request(2, "api")).await?;
    let before = s.load(&id(1)).await?;
    f.fail_after(&s, WriteStage::DerivedRows).await?;
    assert!(
        s.commit(batch(1, 0, 1, ExecutionStatus::Succeeded))
            .await
            .is_err()
    );
    assert_eq!(s.load(&id(1)).await?, before);
    assert!(s.list_runs().await?.is_empty());
    assert!(s.list_assignments().await?.is_empty());
    assert_eq!(
        s.admission(&id(1)).await?.unwrap().state,
        AdmissionState::Admitted
    );
    assert_eq!(
        s.admission(&id(2)).await?.unwrap().state,
        AdmissionState::Queued
    );
    s.commit(batch(1, 0, 1, ExecutionStatus::Succeeded)).await?;
    settled(f, &s, 1, 1).await?;
    assert_eq!(
        s.admission(&id(2)).await?.unwrap().state,
        AdmissionState::Admitted
    );
    Ok(())
}
pub async fn derived_rows_cannot_release_running_execution<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    s.admit(request(1, "api")).await?;
    s.admit(request(2, "api")).await?;
    let before = s.load(&id(1)).await?;
    for mode in 0..3 {
        let mut bad = batch(1, 0, 1, ExecutionStatus::Running);
        match mode {
            0 => bad.derived_outbox[0].release_concurrency = true,
            1 => bad.derived_outbox[0].summary.status = ExecutionStatus::Succeeded,
            _ => bad.derived_outbox[0].summary.completed_at_ms = Some(10),
        }
        assert!(s.commit(bad).await.is_err());
        assert_eq!(s.load(&id(1)).await?, before);
        assert_eq!(
            s.admission(&id(2)).await?.unwrap().state,
            AdmissionState::Queued
        );
        assert!(s.list_runs().await?.is_empty());
    }
    Ok(())
}

/// D25: a start's limit replaces the pool limit; raising promotes waiters
/// in the same transaction, lowering preempts nobody.
pub async fn pool_limit_replacement_promotes_waiters<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    let request = |n: u8, limit: u32| {
        let mut r = request(n, "api");
        r.limit = limit;
        r
    };
    assert_eq!(
        s.admit(request(1, 1)).await?.state,
        AdmissionState::Admitted
    );
    assert_eq!(s.admit(request(2, 1)).await?.state, AdmissionState::Queued);
    // Raising to 2 promotes the waiter ahead of the new request.
    assert_eq!(s.admit(request(3, 2)).await?.state, AdmissionState::Queued);
    let promoted = s.admission(&id(2)).await?.unwrap();
    assert_eq!(promoted.state, AdmissionState::Admitted);
    assert!(promoted.admitted_at_ms.is_some());
    // Lowering back to 1 preempts nobody and admits no one new.
    assert_eq!(s.admit(request(4, 1)).await?.state, AdmissionState::Queued);
    assert_eq!(
        s.admission(&id(1)).await?.unwrap().state,
        AdmissionState::Admitted
    );
    assert_eq!(
        s.admission(&id(2)).await?.unwrap().state,
        AdmissionState::Admitted
    );
    // F30: two admitted under limit 1. Ending one leaves the pool at its
    // limit, so the release admits no one new.
    s.commit(batch(1, 0, 1, ExecutionStatus::Succeeded)).await?;
    settled(f, &s, 1, 1).await?;
    for n in [3, 4] {
        assert_eq!(
            s.admission(&id(n)).await?.unwrap().state,
            AdmissionState::Queued
        );
    }
    // Ending the other drops the count below the limit: the oldest waiter runs.
    s.commit(batch(2, 0, 1, ExecutionStatus::Succeeded)).await?;
    settled(f, &s, 2, 1).await?;
    assert_eq!(
        s.admission(&id(3)).await?.unwrap().state,
        AdmissionState::Admitted
    );
    assert_eq!(
        s.admission(&id(4)).await?.unwrap().state,
        AdmissionState::Queued
    );
    Ok(())
}
