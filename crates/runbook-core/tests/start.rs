//! B5a on the memory store: version pinning, queueing, and activation
//! anchored at admission time under a fake clock.
use runbook_core::{
    ExecutionStore,
    blob::BlobKey,
    blob::BlobStore,
    catalog::{AdmissionState, CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, activate, prepare_start_commit},
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{Data, ExecutionId, RunbookId, VersionId};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory store must be synchronous"),
    }
}
fn scope() -> TenantScope {
    TenantScope {
        org_id: "org_00000000000000000000000001".parse().unwrap(),
        project_id: "prj_00000000000000000000000001".parse().unwrap(),
    }
}
fn rb() -> RunbookId {
    "rb_00000000000000000000000001".parse().unwrap()
}
fn exe(n: u8) -> ExecutionId {
    format!("exe_{n:026}").parse().unwrap()
}
const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: deploy-check, title: Deploy check}\n\
    spec:\n  execution: {timeout: 2h, concurrency: {key: \"${{ inputs.service }}\", limit: 1, on_conflict: queue}}\n  \
    inputs: {service: {type: string, required: true}}\n  \
    steps: [{id: only, title: Only}]\n";

#[test]
fn memory_start_queue_and_activation_anchor_at_admission() {
    let projection = runbook_core::ExecutionProjection {
        execution_id: exe(90),
        runbook_id: rb(),
        version_id: "rbv_00000000000000000000000090".parse().unwrap(),
        compiled_hash: [0; 32],
        engine_version: runbook_engine::ENGINE_VERSION,
        sequence: 0,
        status: runbook_model::ExecutionStatus::Queued,
        phase: runbook_engine::ExecutionPhase::Ordinary,
        completed_at_ms: None,
        result: serde_json::Value::Null,
        outputs: Default::default(),
        steps: vec![],
    };
    let store = MemoryExecutionStore::new(scope(), projection, 1_000);
    ready(store.create_runbook(
        RunbookRecord {
            runbook_id: rb(),
            slug: "deploy-check".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        SPEC.into(),
    ))
    .unwrap();
    let prepared = prepare(
        "deploy-check",
        SPEC,
        &runbook_spec::CompileContext::default(),
        "rbv_00000000000000000000000001"
            .parse::<VersionId>()
            .unwrap(),
    )
    .unwrap();
    ready(publish_draft(&store, &store, &rb(), 1, prepared)).unwrap();
    let command = |n: u8| StartCommand {
        execution_id: exe(n),
        runbook_id: rb(),
        version: None,
        inputs: Data::from([("service".to_string(), serde_json::json!("api"))]),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: format!("k{n}"),
        principal: "user:alice".into(),
        request_hash: [n; 32],
    };
    let first = ready(prepare_start_commit(&store, &store, &command(1))).unwrap();
    let admitted = ready(store.commit_start(first)).unwrap();
    assert_eq!(admitted.admission.unwrap().state, AdmissionState::Admitted);
    let queued_commit = ready(prepare_start_commit(&store, &store, &command(2))).unwrap();
    let queued = ready(store.commit_start(queued_commit)).unwrap();
    assert_eq!(queued.admission.unwrap().state, AdmissionState::Queued);
    // Two hours in the queue spend none of the two-hour deadline.
    store.set_time(1_000 + 2 * 3_600_000);
    // The queued run offers only its `execution.queued` notification (C1).
    let claims: Vec<_> = ready(store.claim_due(10, 60_000))
        .unwrap()
        .into_iter()
        .filter(|lease| !matches!(lease.id.kind, WorkKind::Notification { .. }))
        .collect();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].id.kind, WorkKind::Activation);
    assert_eq!(claims[0].id.execution_id, exe(1));
    // Terminate the first run through a derived release, promoting run 2
    // at the advanced clock; activation anchors there.
    ready(activate(
        &store,
        &runbook_core::directory::memory::MemoryDirectory::default(),
        scope(),
        &claims[0],
    ))
    .unwrap();
    let run = ready(store.load(&exe(1))).unwrap();
    let mut projection = run.projection.clone();
    projection.sequence = run.revision + 1;
    projection.status = runbook_model::ExecutionStatus::Succeeded;
    projection.completed_at_ms = Some(1_000 + 2 * 3_600_000);
    ready(store.commit(runbook_core::ExecutionCommit {
        execution_id: exe(1),
        expected_sequence: run.revision,
        events: vec![runbook_engine::EventEnvelope {
            execution_id: exe(1),
            seq: run.revision + 1,
            at_ms: 1_000 + 2 * 3_600_000,
            actor: runbook_model::ActorRef::System,
            event: runbook_engine::Event::ManualPauseChanged { paused: false },
        }],
        projection: projection.clone(),
        timers: vec![],
        outbox: vec![],
        idempotency: vec![],
        claims: vec![],
        credentials: vec![],
        derived_outbox: vec![runbook_core::catalog::DerivedUpdate {
            summary: runbook_core::catalog::RunSummary {
                execution_id: exe(1),
                runbook_id: rb(),
                sequence: run.revision + 1,
                status: runbook_model::ExecutionStatus::Succeeded,
                completed_at_ms: Some(1_000 + 2 * 3_600_000),
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
    }))
    .unwrap();
    let promoted = ready(store.admission(&exe(2))).unwrap().unwrap();
    assert_eq!(promoted.state, AdmissionState::Admitted);
    assert_eq!(promoted.admitted_at_ms, Some(1_000 + 2 * 3_600_000));
    let claim = ready(store.claim_due(10, 60_000))
        .unwrap()
        .into_iter()
        .find(|lease| lease.id.kind == WorkKind::Activation && lease.id.execution_id == exe(2))
        .expect("activation work for the promoted run");
    ready(activate(
        &store,
        &runbook_core::directory::memory::MemoryDirectory::default(),
        scope(),
        &claim,
    ))
    .unwrap();
    let run_b = ready(store.load(&exe(2))).unwrap();
    assert!(run_b.events.len() >= 2, "started plus settled ready steps");
    assert_eq!(run_b.events[0].at_ms, 1_000 + 2 * 3_600_000);
    assert_eq!(run_b.projection.steps.len(), 1);
    assert!(run_b.timers.contains_key("execution-deadline"));
    let record = ready(store.start_record(&exe(2))).unwrap().unwrap();
    let bytes = ready(store.get(&BlobKey::Shared {
        hash: record.snapshot_digest,
    }))
    .unwrap();
    let compiled = runbook_spec::restore(&bytes).unwrap();
    let state = runbook_engine::ExecutionState::new(exe(2), std::rc::Rc::new(compiled));
    let started = run_b
        .events
        .iter()
        .try_fold(state, |state, event| runbook_engine::apply(&state, event))
        .unwrap();
    assert_eq!(started.started_at_ms, Some(1_000 + 2 * 3_600_000));
    assert_eq!(
        started.execution_deadline_at_ms,
        Some(1_000 + 4 * 3_600_000)
    );
}

const PLAIN: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: plain-check, title: Plain check}\n\
    spec: {steps: [{id: only, title: Only}]}\n";

#[test]
fn memory_unpooled_replay_expiry_and_execution_project() {
    let projection = runbook_core::ExecutionProjection {
        execution_id: exe(90),
        runbook_id: rb(),
        version_id: "rbv_00000000000000000000000090".parse().unwrap(),
        compiled_hash: [0; 32],
        engine_version: runbook_engine::ENGINE_VERSION,
        sequence: 0,
        status: runbook_model::ExecutionStatus::Queued,
        phase: runbook_engine::ExecutionPhase::Ordinary,
        completed_at_ms: None,
        result: serde_json::Value::Null,
        outputs: Default::default(),
        steps: vec![],
    };
    let store = MemoryExecutionStore::new(scope(), projection, 5_000);
    ready(store.create_runbook(
        RunbookRecord {
            runbook_id: rb(),
            slug: "plain-check".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        PLAIN.into(),
    ))
    .unwrap();
    let prepared = prepare(
        "plain-check",
        PLAIN,
        &runbook_spec::CompileContext::default(),
        "rbv_00000000000000000000000001".parse().unwrap(),
    )
    .unwrap();
    ready(publish_draft(&store, &store, &rb(), 1, prepared)).unwrap();
    let command = |n: u8, hash: u8| StartCommand {
        execution_id: exe(n),
        runbook_id: rb(),
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: "same-key".into(),
        principal: "user:alice".into(),
        request_hash: [hash; 32],
    };
    let commit = ready(prepare_start_commit(&store, &store, &command(1, 1))).unwrap();
    let outcome = ready(store.commit_start(commit)).unwrap();
    assert_eq!(outcome.admission, None);
    // Unpooled runs still get activation work anchored at start time.
    let claim = ready(store.claim_due(1, 60_000)).unwrap().remove(0);
    assert_eq!(claim.id.kind, WorkKind::Activation);
    ready(activate(
        &store,
        &runbook_core::directory::memory::MemoryDirectory::default(),
        scope(),
        &claim,
    ))
    .unwrap();
    let run = ready(store.load(&exe(1))).unwrap();
    assert_eq!(run.events[0].at_ms, 5_000);
    // Routing read: same org resolves, foreign org does not.
    assert_eq!(
        ready(store.execution_project(&scope().org_id, &exe(1))).unwrap(),
        Some(scope().project_id)
    );
    let foreign_org = "org_00000000000000000000000009".parse().unwrap();
    assert_eq!(
        ready(store.execution_project(&foreign_org, &exe(1))).unwrap(),
        None
    );
    // Replay retention: inside 24h it is findable; past it, gone.
    let found = ready(store.find_replay("same-key")).unwrap().unwrap();
    assert_eq!(found.outcome.execution_id, exe(1));
    store.set_time(5_000 + 25 * 3_600_000);
    assert_eq!(ready(store.find_replay("same-key")).unwrap(), None);
    let fresh = ready(prepare_start_commit(&store, &store, &command(2, 9))).unwrap();
    assert_eq!(
        ready(store.commit_start(fresh)).unwrap().execution_id,
        exe(2)
    );
}

/// CF12b review round 1 (finding 7): the API caps the start data a request
/// carries (256 KiB), but the start record also keeps the version's input
/// defaults, which a source of up to 1 MiB can make nearly 1 MiB. With
/// large start data that passes the 2,000,000 bytes a D1 or Durable Object
/// row holds; core measures the merged record and refuses such a start
/// (`request.value_too_large`, nothing written) instead of a write that
/// can never succeed.
#[test]
fn a_start_whose_merged_record_passes_a_row_is_refused() {
    use runbook_core::start::{MAX_START_ROW_BYTES, StartError};
    let projection = runbook_core::ExecutionProjection {
        execution_id: exe(90),
        runbook_id: rb(),
        version_id: "rbv_00000000000000000000000090".parse().unwrap(),
        compiled_hash: [0; 32],
        engine_version: runbook_engine::ENGINE_VERSION,
        sequence: 0,
        status: runbook_model::ExecutionStatus::Queued,
        phase: runbook_engine::ExecutionPhase::Ordinary,
        completed_at_ms: None,
        result: serde_json::Value::Null,
        outputs: Default::default(),
        steps: vec![],
    };
    let store = MemoryExecutionStore::new(scope(), projection, 1_000);
    // Fifteen inputs whose defaults are 64,000 bytes each (960 KB), and
    // sixteen string fields the start fills.
    let default = "d".repeat(64_000);
    let inputs: String = (0..15)
        .map(|i| format!("    i{i}: {{type: string, default: \"{default}\"}}\n"))
        .collect();
    let fields: String = (0..16)
        .map(|i| format!("    f{i}: {{type: string}}\n"))
        .collect();
    let spec = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: big-defaults, title: Big defaults}}\n\
         spec:\n  inputs:\n{inputs}  fields:\n{fields}  steps: [{{id: only, title: Only}}]\n"
    );
    ready(store.create_runbook(
        RunbookRecord {
            runbook_id: rb(),
            slug: "big-defaults".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        spec.clone(),
    ))
    .unwrap();
    let prepared = prepare(
        "big-defaults",
        &spec,
        &runbook_spec::CompileContext::default(),
        "rbv_00000000000000000000000001"
            .parse::<VersionId>()
            .unwrap(),
    )
    .unwrap();
    ready(publish_draft(&store, &store, &rb(), 1, prepared)).unwrap();
    let command = |n: u8, field_bytes: usize| StartCommand {
        execution_id: exe(n),
        runbook_id: rb(),
        version: None,
        inputs: Data::new(),
        fields: (0..16)
            .map(|i| (format!("f{i}"), serde_json::json!("f".repeat(field_bytes))))
            .collect(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: format!("k{n}"),
        principal: "user:alice".into(),
        request_hash: [n; 32],
    };
    // Sixteen fields of 16 KiB (256 KiB, the API's cap) fit beside the
    // defaults.
    let small = ready(prepare_start_commit(&store, &store, &command(1, 16_384))).unwrap();
    assert!(serde_json::to_vec(&small.record).unwrap().len() <= MAX_START_ROW_BYTES);
    // Sixteen of 64,000 bytes (1 MB): the merged record passes a row.
    let Err(error) = ready(prepare_start_commit(&store, &store, &command(2, 64_000))) else {
        panic!("refused");
    };
    assert!(
        matches!(
            error,
            StartError::Store(runbook_core::StoreError::InvalidBatch(
                "request.value_too_large"
            ))
        ),
        "{error:?}"
    );
}
