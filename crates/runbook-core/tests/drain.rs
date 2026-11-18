//! B4.4 on the memory store under a fake clock: a timer lease applies exactly
//! one transition, survives a crash between commit and acknowledgement, and
//! an injected write failure leaves the run unchanged and the timer retryable.
use runbook_core::{
    ExecutionStore, WriteStage,
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, memory::MemoryDirectory,
    },
    drain::{Processed, process_lease},
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, prepare_start_commit},
    work_queue::{WorkKind, WorkLease, WorkQueue},
};
use runbook_model::{Data, ExecutionId, ExecutionStatus, RunbookId};
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
fn exe() -> ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}
const START_MS: i64 = 1_000;
/// A one-second execution deadline: the only timer, firing expires the run.
const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: timed, title: Timed}\n\
    spec:\n  execution: {timeout: 1s}\n  steps: [{id: only, title: Only}]\n";

/// Publish, start and activate one run through the drain itself.
fn started() -> (MemoryExecutionStore, MemoryDirectory) {
    started_with(SPEC, vec![], Default::default())
}
fn started_with(
    spec: &str,
    directory_setup: Vec<DirectoryMutation>,
    context: runbook_spec::CompileContext,
) -> (MemoryExecutionStore, MemoryDirectory) {
    let projection = runbook_core::ExecutionProjection {
        execution_id: "exe_00000000000000000000000090".parse().unwrap(),
        runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
        version_id: "rbv_00000000000000000000000090".parse().unwrap(),
        compiled_hash: [0; 32],
        engine_version: runbook_engine::ENGINE_VERSION,
        sequence: 0,
        status: ExecutionStatus::Queued,
        phase: runbook_engine::ExecutionPhase::Ordinary,
        completed_at_ms: None,
        result: serde_json::Value::Null,
        outputs: Default::default(),
        steps: vec![],
    };
    let store = MemoryExecutionStore::new(scope(), projection, START_MS);
    let directory = MemoryDirectory::default();
    ready(
        directory.commit(DirectoryCommit {
            org_id: scope().org_id,
            expected_revision: 0,
            mutations: vec![
                DirectoryMutation::CreateOrganization {
                    title: "Org".into(),
                },
                DirectoryMutation::CreateProject(ProjectRecord {
                    id: scope().project_id,
                    org_id: scope().org_id,
                    slug: "api".into(),
                    title: "API".into(),
                }),
            ]
            .into_iter()
            .chain(directory_setup)
            .collect(),
        }),
    )
    .unwrap();
    let rb: RunbookId = "rb_00000000000000000000000001".parse().unwrap();
    ready(store.create_runbook(
        RunbookRecord {
            runbook_id: rb.clone(),
            slug: "timed".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        spec.into(),
    ))
    .unwrap();
    let prepared = prepare(
        "timed",
        spec,
        &context,
        "rbv_00000000000000000000000001".parse().unwrap(),
    )
    .unwrap();
    ready(publish_draft(&store, &store, &rb, 1, prepared)).unwrap();
    let command = StartCommand {
        execution_id: exe(),
        runbook_id: rb,
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: "k1".into(),
        principal: "user:alice".into(),
        request_hash: [1; 32],
    };
    let commit = ready(prepare_start_commit(&store, &store, &command)).unwrap();
    ready(store.commit_start(commit)).unwrap();
    let activation = ready(store.claim_due(10, 60_000)).unwrap().remove(0);
    assert_eq!(activation.id.kind, WorkKind::Activation);
    assert_eq!(
        ready(process_lease(&store, &directory, scope(), &activation)).unwrap(),
        Processed::Activated
    );
    let run = ready(store.load(&exe())).unwrap();
    assert!(
        run.timers.contains_key("execution-deadline"),
        "{:?}",
        run.timers
    );
    (store, directory)
}

fn deadline_lease(store: &MemoryExecutionStore) -> WorkLease {
    ready(store.claim_due(10, 60_000))
        .unwrap()
        .into_iter()
        .find(|lease| {
            matches!(&lease.id.kind, WorkKind::Timer { key, .. } if key == "execution-deadline")
        })
        .expect("the execution deadline is due")
}

fn terminal(status: ExecutionStatus) -> bool {
    matches!(
        status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
}

#[test]
fn timer_restart_applies_one_transition() {
    let (store, directory) = started();
    let before = ready(store.load(&exe())).unwrap();
    assert!(!terminal(before.projection.status));
    // Nothing is due before the deadline.
    assert!(
        ready(store.claim_due(10, 60_000))
            .unwrap()
            .iter()
            .all(|lease| !matches!(lease.id.kind, WorkKind::Timer { .. }))
    );

    store.set_time(START_MS + 1_500);
    let lease = deadline_lease(&store);
    assert_eq!(
        ready(process_lease(&store, &directory, scope(), &lease)).unwrap(),
        Processed::Fired
    );
    let fired = ready(store.load(&exe())).unwrap();
    assert!(
        terminal(fired.projection.status),
        "{:?}",
        fired.projection.status
    );
    assert!(fired.revision > before.revision);
    assert!(
        fired
            .events
            .iter()
            .all(|e| e.at_ms == START_MS + 1_500 || e.at_ms <= START_MS),
        "transitions carry the store's post-lock time"
    );
    assert!(!fired.timers.contains_key("execution-deadline"));

    // A second worker still holding the same lease after a crash between
    // commit and acknowledgement applies nothing.
    assert_eq!(
        ready(process_lease(&store, &directory, scope(), &lease)).unwrap(),
        Processed::Obsolete
    );
    let after = ready(store.load(&exe())).unwrap();
    assert_eq!(after.revision, fired.revision);
    assert_eq!(after.events, fired.events);
    assert_eq!(after.outbox, fired.outbox);
    // And no timer work remains to be claimed.
    store.set_time(START_MS + 10_000);
    assert!(
        ready(store.claim_due(10, 60_000))
            .unwrap()
            .iter()
            .all(|lease| !matches!(lease.id.kind, WorkKind::Timer { .. }))
    );
}

#[test]
fn timer_failure_rolls_back_and_retries() {
    let (store, directory) = started();
    store.set_time(START_MS + 1_500);
    let lease = deadline_lease(&store);
    let before = ready(store.load(&exe())).unwrap();

    store.fail_after(WriteStage::Events);
    assert!(ready(process_lease(&store, &directory, scope(), &lease)).is_err());
    let unchanged = ready(store.load(&exe())).unwrap();
    assert_eq!(
        unchanged, before,
        "an injected write failure changes nothing"
    );

    // The lease expires; the same timer is claimed again and applies once.
    store.set_time(START_MS + 1_500 + 60_001);
    let retry = deadline_lease(&store);
    assert!(retry.generation > lease.generation);
    assert_eq!(
        ready(process_lease(&store, &directory, scope(), &retry)).unwrap(),
        Processed::Fired
    );
    let fired = ready(store.load(&exe())).unwrap();
    assert!(terminal(fired.projection.status));
    // The stale first lease can neither apply nor acknowledge anything now.
    assert_eq!(
        ready(process_lease(&store, &directory, scope(), &lease)).unwrap(),
        Processed::Obsolete
    );
    assert_eq!(ready(store.load(&exe())).unwrap(), fired);
}

#[test]
fn notification_work_is_left_for_delivery() {
    let (store, directory) = started();
    let lease = WorkLease {
        scope: scope(),
        id: runbook_core::work_queue::WorkId {
            execution_id: exe(),
            kind: WorkKind::Notification {
                event_id: "evt_x".into(),
            },
        },
        generation: 1,
        deadline_at_ms: START_MS + 60_000,
    };
    let before = ready(store.load(&exe())).unwrap();
    assert_eq!(
        ready(process_lease(&store, &directory, scope(), &lease)).unwrap(),
        Processed::Skipped
    );
    assert_eq!(ready(store.load(&exe())).unwrap(), before);
}

/// C4: an escalating approval's expiry must be re-armed after the
/// escalation fires. The next wakeup ignores finished steps and the
/// approval's first deadline once it has escalated.
#[test]
fn escalated_approval_expires_from_the_drain() {
    use runbook_core::directory::{OrgRole, TeamRecord, UserRecord};
    let team: runbook_model::TeamId = "team_00000000000000000000000001".parse().unwrap();
    let user: runbook_model::UserId = "usr_00000000000000000000000001".parse().unwrap();
    let spec = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
        metadata: {id: timed, title: Timed}\n\
        spec:\n  execution: {timeout: 24h}\n  steps:\n    \
        - id: gate\n      approval: {prompt: Go?, options: [keep], approvers: {teams: [oncall]}, \
        timeout: 2s, escalate_to: [team:oncall], escalation_timeout: 2s}\n";
    let (store, directory) = started_with(
        spec,
        vec![
            DirectoryMutation::CreateTeam(TeamRecord {
                id: team.clone(),
                org_id: scope().org_id,
                slug: "oncall".into(),
                title: "Oncall".into(),
            }),
            DirectoryMutation::CreateUser(UserRecord {
                id: user.clone(),
                name: "Alice".into(),
                active: true,
                role: OrgRole::Member,
            }),
            DirectoryMutation::AddTeamMember {
                team_id: team.clone(),
                user_id: user,
            },
        ],
        runbook_spec::CompileContext {
            teams: [("oncall".into(), team)].into(),
            ..Default::default()
        },
    );
    let fire_due = |at: i64| {
        store.set_time(at);
        let leases: Vec<_> = ready(store.claim_due(10, 60_000))
            .unwrap()
            .into_iter()
            .filter(|lease| matches!(lease.id.kind, WorkKind::Timer { .. }))
            .collect();
        for lease in &leases {
            ready(process_lease(&store, &directory, scope(), lease)).unwrap();
        }
        leases.len()
    };
    let gate_status = || {
        let run = ready(store.load(&exe())).unwrap();
        run.projection
            .steps
            .iter()
            .find(|s| s.path == "gate")
            .map(|s| s.status)
    };
    assert_eq!(gate_status(), Some(runbook_model::StepStatus::Waiting));
    let base = START_MS;
    // Escalation at +2 s, then expiry at +4 s, each from a drained timer.
    assert!(fire_due(base + 2_100) > 0, "escalation is due");
    let escalated = ready(store.load(&exe())).unwrap();
    assert!(
        escalated
            .events
            .iter()
            .any(|e| matches!(e.event, runbook_engine::Event::ApprovalEscalated { .. })),
        "escalated"
    );
    assert!(
        fire_due(base + 4_200) > 0,
        "expiry is re-armed after escalation: {:?}",
        ready(store.load(&exe())).unwrap().timers
    );
    assert_eq!(gate_status(), Some(runbook_model::StepStatus::Expired));
}
