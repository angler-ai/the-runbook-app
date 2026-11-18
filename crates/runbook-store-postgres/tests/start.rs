//! B5a storage-level start admission: version pinning, FIFO queueing,
//! replay atomicity, and activation anchoring the deadline at admission.
mod support;
use runbook_core::{
    ExecutionStore, StoreError,
    blob::BlobStore,
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord},
    publish::{prepare, publish_draft},
    start::{StartCommand, StartError, StartStore, activate, prepare_start_commit},
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{Data, ExecutionId, ExecutionStatus, RunbookId, VersionId, VersionSelector};
use runbook_store_conformance::{WorkQueueFactory, scope};
use runbook_store_postgres::PostgresExecutionStore;
use std::num::NonZeroU32;
use support::factory::{PostgresFactory, with_factory};

fn rb(n: u8) -> RunbookId {
    format!("rb_{n:026}").parse().unwrap()
}
fn rbv(n: u8) -> VersionId {
    format!("rbv_{n:026}").parse().unwrap()
}
fn exe(n: u8) -> ExecutionId {
    format!("exe_{n:026}").parse().unwrap()
}
fn spec(step_title: &str) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: deploy-check, title: Deploy check}}\n\
         spec:\n  \
         execution: {{timeout: 2h, concurrency: {{key: \"${{{{ inputs.service }}}}\", limit: 1, on_conflict: queue}}}}\n  \
         inputs: {{service: {{type: string, required: true}}, region: {{type: string, default: us}}}}\n  \
         steps: [{{id: only, title: {step_title}}}]\n"
    )
}
async fn publish(
    store: &PostgresExecutionStore,
    revision: u64,
    source: &str,
    version: u8,
) -> runbook_model::PublishedVersionView {
    if revision > 1 {
        store
            .save_draft(&rb(1), revision - 1, source.into())
            .await
            .unwrap();
    }
    let prepared = prepare(
        "deploy-check",
        source,
        &runbook_spec::CompileContext::default(),
        rbv(version),
    )
    .unwrap();
    publish_draft(store, store, &rb(1), revision, prepared)
        .await
        .unwrap()
}
async fn setup(f: &PostgresFactory) -> PostgresExecutionStore {
    let store = f.fresh_store(scope(1)).await;
    store
        .create_runbook(
            RunbookRecord {
                runbook_id: rb(1),
                slug: "deploy-check".into(),
                origin: RunbookOrigin::Ui,
                archived: false,
            },
            spec("One"),
        )
        .await
        .unwrap();
    store
}
fn command(n: u8, version: Option<VersionSelector>, key: &str) -> StartCommand {
    StartCommand {
        execution_id: exe(n),
        runbook_id: rb(1),
        version,
        inputs: Data::from([("service".to_string(), serde_json::json!("api"))]),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: key.into(),
        principal: "user:alice".into(),
        request_hash: [n; 32],
    }
}
async fn start(
    store: &PostgresExecutionStore,
    cmd: &StartCommand,
) -> Result<runbook_core::start::StartOutcome, StartError> {
    let commit = prepare_start_commit(store, store, cmd).await?;
    Ok(store.commit_start(commit).await?)
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn start_pins_version_before_queue() {
    with_factory(async |f| {
        let store = setup(f).await;
        let v1 = publish(&store, 1, &spec("One"), 1).await;
        let v2 = publish(&store, 2, &spec("Two"), 2).await;
        let first = start(&store, &command(1, None, "k1")).await.unwrap();
        assert_eq!(first.runbook_version_id, v2.runbook_version_id);
        assert_eq!(
            first.admission.as_ref().unwrap().state,
            runbook_core::catalog::AdmissionState::Admitted
        );
        let explicit = start(
            &store,
            &command(
                2,
                Some(VersionSelector::Number(NonZeroU32::new(1).unwrap())),
                "k2",
            ),
        )
        .await
        .unwrap();
        assert_eq!(explicit.runbook_version_id, v1.runbook_version_id);
        assert_eq!(explicit.runbook_version, NonZeroU32::new(1));
        assert_eq!(
            explicit.admission.unwrap().state,
            runbook_core::catalog::AdmissionState::Queued
        );
        // A preview start records its rbv_ with a null number.
        let preview = runbook_core::publish::publish_preview(
            &store,
            &store,
            &rb(1),
            prepare(
                "deploy-check",
                &spec("Preview"),
                &runbook_spec::CompileContext::default(),
                rbv(7),
            )
            .unwrap(),
            Some("feature".into()),
            None,
        )
        .await?;
        let previewed = start(
            &store,
            &command(
                3,
                Some(VersionSelector::Id(preview.runbook_version_id.clone())),
                "k3",
            ),
        )
        .await
        .unwrap();
        assert_eq!(previewed.runbook_version, None);
        // Unknown and foreign versions are the same not-found.
        assert!(matches!(
            start(
                &store,
                &command(
                    4,
                    Some(VersionSelector::Number(NonZeroU32::new(9).unwrap())),
                    "k4"
                )
            )
            .await,
            Err(StartError::VersionNotFound)
        ));
        let foreign = f.fresh_store(scope(2)).await;
        assert!(matches!(
            start(&foreign, &command(5, None, "k5")).await,
            Err(StartError::VersionNotFound)
        ));
        // Publishing v3 does not move queued pins, inputs or the key.
        publish(&store, 3, &spec("Three"), 3).await;
        let record = store.start_record(&exe(2)).await?.unwrap();
        assert_eq!(record.version_id, v1.runbook_version_id);
        assert_eq!(record.version, NonZeroU32::new(1));
        assert_eq!(record.context.inputs["region"], serde_json::json!("us"));
        assert_eq!(record.concurrency_key.as_deref(), Some("api"));
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn start_replay_is_atomic_and_authorized() {
    with_factory(async |f| {
        let store = setup(f).await;
        publish(&store, 1, &spec("One"), 1).await;
        let other = f.fresh_store(scope(1)).await;
        let cmd = command(1, None, "same-key");
        let prepared_a = prepare_start_commit(&store, &store, &cmd).await.unwrap();
        let prepared_b = prepare_start_commit(&other, &other, &cmd).await.unwrap();
        let (a, b) = tokio::join!(
            store.commit_start(prepared_a),
            other.commit_start(prepared_b)
        );
        assert_eq!(a.unwrap(), b.unwrap());
        // One execution, one slot: the next start queues behind it.
        let next = start(&store, &command(2, None, "k2")).await.unwrap();
        assert_eq!(
            next.admission.unwrap().state,
            runbook_core::catalog::AdmissionState::Queued
        );
        // Same key with a changed payload hash is a conflict, and a retry
        // after a newer publish still returns the original outcome.
        publish(&store, 2, &spec("Two"), 2).await;
        let retry = prepare_start_commit(&store, &store, &cmd).await.unwrap();
        let replayed = store.commit_start(retry).await?;
        assert_eq!(replayed.execution_id, exe(1));
        assert_eq!(replayed.runbook_version, NonZeroU32::new(1));
        let mut changed = cmd.clone();
        changed.request_hash = [9; 32];
        changed.execution_id = exe(9);
        let changed_commit = prepare_start_commit(&store, &store, &changed)
            .await
            .unwrap();
        assert_eq!(
            store.commit_start(changed_commit).await,
            Err(StoreError::InvalidBatch("request.idempotency_key_reused"))
        );
        assert_eq!(store.load(&exe(9)).await, Err(StoreError::NotFound));
        Ok(())
    })
    .await;
}

async fn terminate(store: &PostgresExecutionStore, id: &ExecutionId) -> Result<(), StoreError> {
    use runbook_core::catalog::{DerivedUpdate, RunSummary};
    use runbook_engine::{Event, EventEnvelope};
    let run = store.load(id).await?;
    let mut projection = run.projection.clone();
    projection.sequence = run.revision + 1;
    projection.status = ExecutionStatus::Succeeded;
    projection.completed_at_ms = Some(9_999_999);
    store
        .commit(runbook_core::ExecutionCommit {
            execution_id: id.clone(),
            expected_sequence: run.revision,
            events: vec![EventEnvelope {
                execution_id: id.clone(),
                seq: run.revision + 1,
                at_ms: 9_999_999,
                actor: runbook_model::ActorRef::System,
                event: Event::ManualPauseChanged { paused: false },
            }],
            projection: projection.clone(),
            timers: vec![],
            outbox: vec![],
            idempotency: vec![],
            claims: vec![],
            credentials: vec![],
            derived_outbox: vec![DerivedUpdate {
                summary: RunSummary {
                    execution_id: id.clone(),
                    runbook_id: projection.runbook_id.clone(),
                    sequence: projection.sequence,
                    status: ExecutionStatus::Succeeded,
                    completed_at_ms: Some(9_999_999),
                    title: String::new(),
                    tags: vec![],
                    fields: Default::default(),
                    created_by: String::new(),
                    created_at_ms: 0,
                },
                assignments: vec![],
                release_concurrency: true,
                memory: vec![],
            }],
        })
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn queue_limit_fifo_and_restart() {
    use runbook_core::catalog::AdmissionState;
    with_factory(async |f| {
        let store = setup(f).await;
        publish(&store, 1, &spec("One"), 1).await;
        let a = start(&store, &command(1, None, "k1")).await.unwrap();
        let b = start(&store, &command(2, None, "k2")).await.unwrap();
        let c = start(&store, &command(3, None, "k3")).await.unwrap();
        assert_eq!(a.admission.unwrap().state, AdmissionState::Admitted);
        assert_eq!(b.admission.unwrap().state, AdmissionState::Queued);
        assert_eq!(c.admission.unwrap().state, AdmissionState::Queued);
        // Another key proceeds independently.
        let mut other_key = command(4, None, "k4");
        other_key.inputs = Data::from([("service".to_string(), serde_json::json!("web"))]);
        assert_eq!(
            start(&store, &other_key)
                .await
                .unwrap()
                .admission
                .unwrap()
                .state,
            AdmissionState::Admitted
        );
        // Restart preserves FIFO state.
        drop(store);
        let store = f.fresh_store(scope(1)).await;
        assert_eq!(
            store.admission(&exe(2)).await?.unwrap().state,
            AdmissionState::Queued
        );
        // Cancelling a waiter removes only it; the slot count is untouched.
        assert!(store.cancel_queued(&exe(3), None).await?);
        // F37: the row stays, released, so activation never picks it up.
        assert_eq!(
            store.admission(&exe(3)).await?.unwrap().state,
            AdmissionState::Released
        );
        // Terminal closure promotes exactly the oldest remaining waiter.
        terminate(&store, &exe(1)).await?;
        let promoted = store.admission(&exe(2)).await?.unwrap();
        assert_eq!(promoted.state, AdmissionState::Admitted);
        assert!(promoted.admitted_at_ms.is_some());
        let d = start(&store, &command(5, None, "k5")).await.unwrap();
        assert_eq!(d.admission.unwrap().state, AdmissionState::Queued);
        let due = store.claim_due(100, 60_000).await?;
        assert!(
            !due.iter()
                .any(|lease| lease.id.execution_id == exe(3)
                    && lease.id.kind == WorkKind::Activation),
            "a cancelled waiter was offered for activation: {due:?}"
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn queued_time_does_not_spend_deadline() {
    use runbook_core::catalog::AdmissionState;
    with_factory(async |f| {
        let store = setup(f).await;
        publish(&store, 1, &spec("One"), 1).await;
        let _a = start(&store, &command(1, None, "k1")).await.unwrap();
        let b = start(&store, &command(2, None, "k2")).await.unwrap();
        assert_eq!(b.admission.unwrap().state, AdmissionState::Queued);
        // Queued runs materialize no actor steps.
        assert!(store.load(&exe(2)).await?.projection.steps.is_empty());
        // Only the admitted run offers activation work; the queued run
        // offers just its `execution.queued` notification (C1).
        let claims: Vec<_> = store
            .claim_due(10, 60_000)
            .await?
            .into_iter()
            .filter(|lease| !matches!(lease.id.kind, WorkKind::Notification { .. }))
            .collect();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].id.execution_id, exe(1));
        assert_eq!(claims[0].id.kind, WorkKind::Activation);
        activate(&store, &f.fresh_directory(), scope(1), &claims[0]).await?;
        assert!(store.load(&exe(1)).await?.revision >= 2);
        assert!(
            store
                .claim_due(10, 60_000)
                .await?
                .iter()
                .all(|lease| lease.id.kind != WorkKind::Activation)
        );
        // Promote B; crash between promotion and activation, then recover.
        terminate(&store, &exe(1)).await?;
        let admitted = store.admission(&exe(2)).await?.unwrap();
        assert_eq!(admitted.state, AdmissionState::Admitted);
        let admitted_at = admitted.admitted_at_ms.unwrap();
        let first_claim = store.claim_due(1, 5).await?.remove(0);
        f.expire_lease(&store, &first_claim).await?;
        let second_claim = store.claim_due(1, 60_000).await?.remove(0);
        assert_eq!(second_claim.generation, first_claim.generation + 1);
        activate(&store, &f.fresh_directory(), scope(1), &second_claim).await?;
        // The stale worker cannot activate or ack a second time.
        assert!(
            activate(&store, &f.fresh_directory(), scope(1), &first_claim)
                .await
                .is_err()
        );
        let run_b = store.load(&exe(2)).await?;
        assert!(run_b.events.len() >= 2, "started plus settled ready steps");
        assert_eq!(run_b.events[0].at_ms, admitted_at);
        assert_eq!(run_b.projection.steps.len(), 1);
        assert!(run_b.timers.contains_key("execution-deadline"));
        // The deadline anchors at admission, not at queue time.
        let record = store.start_record(&exe(2)).await?.unwrap();
        let bytes = store
            .get(&runbook_core::blob::BlobKey::Shared {
                hash: record.snapshot_digest,
            })
            .await?;
        let compiled = runbook_spec::restore(&bytes).unwrap();
        let state = runbook_engine::ExecutionState::new(exe(2), std::rc::Rc::new(compiled));
        let started = run_b
            .events
            .iter()
            .try_fold(state, |state, event| runbook_engine::apply(&state, event))
            .unwrap();
        assert_eq!(started.started_at_ms, Some(admitted_at));
        assert_eq!(
            started.execution_deadline_at_ms,
            Some(admitted_at + 2 * 3_600_000)
        );
        Ok(())
    })
    .await;
}

const PLAIN: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: plain-check, title: Plain check}\n\
    spec: {steps: [{id: only, title: Only}]}\n";

/// R11: a start with no concurrency key has no admission, yet still gets
/// activation work anchored at its start transaction time.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn unpooled_start_activates_without_admission() {
    with_factory(async |f| {
        let store = f.fresh_store(scope(1)).await;
        store
            .create_runbook(
                RunbookRecord {
                    runbook_id: rb(1),
                    slug: "plain-check".into(),
                    origin: RunbookOrigin::Ui,
                    archived: false,
                },
                PLAIN.into(),
            )
            .await?;
        let prepared = prepare(
            "plain-check",
            PLAIN,
            &runbook_spec::CompileContext::default(),
            rbv(1),
        )
        .unwrap();
        publish_draft(&store, &store, &rb(1), 1, prepared).await?;
        let mut cmd = command(1, None, "plain-1");
        cmd.inputs = Data::new();
        let outcome = start(&store, &cmd).await.unwrap();
        assert_eq!(outcome.admission, None);
        assert_eq!(store.admission(&exe(1)).await?, None);
        let created = store.get_run(&exe(1)).await?.unwrap().created_at_ms;
        assert!(created > 0);
        let claims = store.claim_due(10, 60_000).await?;
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].id.kind, WorkKind::Activation);
        activate(&store, &f.fresh_directory(), scope(1), &claims[0]).await?;
        let run = store.load(&exe(1)).await?;
        assert!(run.revision >= 2);
        assert_eq!(run.events[0].at_ms, created);
        Ok(())
    })
    .await;
}

/// D20 storage half: saved replay is findable before preparation, and an
/// expired key is purged and behaves as new.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn find_replay_and_expiry_behaves_as_new() {
    with_factory(async |f| {
        let store = setup(f).await;
        publish(&store, 1, &spec("One"), 1).await;
        let outcome = start(&store, &command(1, None, "replay-key"))
            .await
            .unwrap();
        let found = store.find_replay("replay-key").await?.unwrap();
        assert_eq!(found.principal, "user:alice");
        assert_eq!(found.request_hash, [1; 32]);
        assert_eq!(found.outcome, outcome);
        assert_eq!(store.find_replay("other-key").await?, None);
        f.admin
            .client()
            .execute(
                "UPDATE runbook.start_replay \
                 SET created_at_ms = created_at_ms - 25 * 3600 * 1000",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(store.find_replay("replay-key").await?, None);
        let count: i64 = f
            .admin
            .client()
            .query_one("SELECT count(*) FROM runbook.start_replay", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 0);
        // After expiry the same key may start afresh, new payload and all.
        let mut fresh = command(2, None, "replay-key");
        fresh.request_hash = [9; 32];
        let again = start(&store, &fresh).await.unwrap();
        assert_eq!(again.execution_id, exe(2));
        Ok(())
    })
    .await;
}

/// Org-scoped routing read: same org resolves across projects, a foreign
/// org gets nothing.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn execution_project_is_org_scoped() {
    with_factory(async |f| {
        let store = setup(f).await;
        publish(&store, 1, &spec("One"), 1).await;
        start(&store, &command(1, None, "k1")).await.unwrap();
        let other_project = runbook_store_postgres::PostgresExecutionStore::new(
            f.config.clone(),
            runbook_core::catalog::TenantScope {
                org_id: scope(1).org_id,
                project_id: scope(2).project_id,
            },
        );
        assert_eq!(
            other_project
                .execution_project(&scope(1).org_id, &exe(1))
                .await?,
            Some(scope(1).project_id)
        );
        assert_eq!(
            store.execution_project(&scope(2).org_id, &exe(1)).await?,
            None
        );
        assert_eq!(
            store.execution_project(&scope(1).org_id, &exe(9)).await?,
            None
        );
        Ok(())
    })
    .await;
}

/// C1: a start queued behind its concurrency key commits its
/// `execution.queued` intent with the start; an admitted start and a
/// replayed start write none.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn queued_start_commits_execution_queued_with_the_start() {
    use runbook_core::catalog::AdmissionState;
    with_factory(async |f| {
        let store = setup(f).await;
        publish(&store, 1, &spec("One"), 1).await;
        let a = start(&store, &command(1, None, "k1")).await.unwrap();
        let b = start(&store, &command(2, None, "k2")).await.unwrap();
        assert_eq!(a.admission.unwrap().state, AdmissionState::Admitted);
        assert_eq!(b.admission.unwrap().state, AdmissionState::Queued);
        assert!(store.load(&exe(1)).await?.outbox.is_empty());
        let queued = store.load(&exe(2)).await?;
        assert_eq!(queued.revision, 0);
        let intents: Vec<_> = queued.outbox.values().cloned().collect();
        assert_eq!(intents.len(), 1, "{intents:?}");
        assert_eq!(intents[0].kind, "execution.queued");
        assert_eq!(intents[0].sequence, 0);
        assert_eq!(
            intents[0].detail,
            Some(serde_json::json!({"status": "queued", "concurrency_key": "api"}))
        );
        start(&store, &command(2, None, "k2")).await.unwrap();
        assert_eq!(store.load(&exe(2)).await?.outbox.len(), 1);
        // The drain offers it as notification work right away.
        let leases = store.claim_due(10, 60_000).await?;
        assert!(leases.iter().any(|l| l.id.execution_id == exe(2)
            && l.id.kind
                == WorkKind::Notification {
                    event_id: intents[0].event_id.clone()
                }));
        Ok(())
    })
    .await;
}
