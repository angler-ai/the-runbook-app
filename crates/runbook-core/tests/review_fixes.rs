//! Regression tests for the 2026-11-11 independent review, group 1 (core),
//! on the memory store under a fake clock.
use runbook_core::{
    ExecutionStore,
    actor::{ActorPrincipal, begin_actor},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    claims::{ClaimStore, claim_step, expire_claim, heartbeat},
    control::cancel,
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, TeamRecord, UserRecord,
        memory::MemoryDirectory,
    },
    drain::process_lease,
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, activate, prepare_start_commit},
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{
    ActorRef, ClaimRequest, Data, ExecutionId, ExecutionStatus, HeartbeatRequest, ProjectRole,
    RunbookId, StepStatus, UserId,
};
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
fn usr(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn human(n: u8) -> ActorPrincipal {
    ActorPrincipal {
        principal: format!("user:{}", usr(n)),
        actor: ActorRef::Human { user_id: usr(n) },
        credential_ref: "pat".into(),
    }
}
fn agent() -> ActorPrincipal {
    ActorPrincipal {
        principal: "agent:svc_ops".into(),
        actor: ActorRef::Agent {
            principal_id: "svc_ops".into(),
            session_id: "s1".into(),
            name: "ops".into(),
            model: None,
        },
        credential_ref: "exec_ops".into(),
    }
}
fn exe() -> ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}
const START_MS: i64 = 1_000;

/// Publish `source` and start one run, activated at START_MS.
fn setup(source: &str) -> (MemoryExecutionStore, MemoryDirectory) {
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
    let mut mutations = vec![
        DirectoryMutation::CreateOrganization {
            title: "Org".into(),
        },
        DirectoryMutation::CreateProject(ProjectRecord {
            id: scope().project_id,
            org_id: scope().org_id,
            slug: "api".into(),
            title: "API".into(),
        }),
        DirectoryMutation::CreateTeam(TeamRecord {
            id: "team_00000000000000000000000001".parse().unwrap(),
            org_id: scope().org_id,
            slug: "oncall".into(),
            title: "Oncall".into(),
        }),
    ];
    for n in [1u8, 2] {
        mutations.push(DirectoryMutation::CreateUser(UserRecord {
            id: usr(n),
            name: format!("user{n}"),
            active: true,
            role: runbook_core::directory::OrgRole::Member,
        }));
        mutations.push(DirectoryMutation::GrantProjectRole {
            project_id: scope().project_id,
            user_id: usr(n),
            role: ProjectRole::Operator,
        });
        mutations.push(DirectoryMutation::AddTeamMember {
            team_id: "team_00000000000000000000000001".parse().unwrap(),
            user_id: usr(n),
        });
    }
    ready(directory.commit(DirectoryCommit {
        org_id: scope().org_id,
        expected_revision: 0,
        mutations,
    }))
    .unwrap();
    let rb: RunbookId = "rb_00000000000000000000000001".parse().unwrap();
    ready(store.create_runbook(
        RunbookRecord {
            runbook_id: rb.clone(),
            slug: "review".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        source.into(),
    ))
    .unwrap();
    let context = runbook_spec::CompileContext {
        teams: [(
            "oncall".into(),
            "team_00000000000000000000000001".parse().unwrap(),
        )]
        .into(),
        users: [("user1".into(), usr(1)), ("user2".into(), usr(2))].into(),
        ..Default::default()
    };
    let prepared = prepare(
        "review",
        source,
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
    let claim = ready(store.claim_due(1, 60_000)).unwrap().remove(0);
    ready(activate(&store, &directory, scope(), &claim)).unwrap();
    (store, directory)
}

const AGENT_TASK: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: review, title: Review}\n\
    spec:\n  steps:\n    - {id: work, title: Work, actor: agent}\n";

fn claim_work(
    store: &MemoryExecutionStore,
    directory: &MemoryDirectory,
) -> runbook_model::ClaimRef {
    let work = ready(begin_actor(store, directory, scope(), &exe())).unwrap();
    let step = work.runtime.state.steps[0].id.clone().unwrap();
    ready(claim_step(
        work,
        &agent(),
        &step,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    ))
    .unwrap()
}
fn lease_offered(store: &MemoryExecutionStore) -> bool {
    ready(store.claim_due(10, 60_000))
        .unwrap()
        .iter()
        .any(|lease| matches!(lease.id.kind, WorkKind::ClaimLease { .. }))
}

// F6: a claim whose step left `Claimed` without a claim write (here a
// cancel) is released with that commit and never becomes a poison lease.
#[test]
fn cancelling_a_claimed_step_releases_its_claim() {
    let (store, directory) = setup(AGENT_TASK);
    let claim = claim_work(&store, &directory);
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let step = work.runtime.state.steps[0].id.clone().unwrap();
    ready(cancel(&store, work, &human(1), None)).unwrap();
    let record = ready(store.claim_record(&exe(), &step)).unwrap().unwrap();
    assert!(record.released);
    store.set_time(START_MS + runbook_core::claims::AGENT_LEASE_MS + 1);
    assert!(!lease_offered(&store));
    assert!(
        ready(heartbeat(
            &store,
            &agent(),
            &exe(),
            &step,
            &HeartbeatRequest { claim },
            None
        ))
        .is_err()
    );
}

// F6: a claim already left unreleased by an earlier release is acknowledged
// at its lease deadline (the release commits without an event) instead of
// being offered forever.
#[test]
fn a_stranded_claim_lease_is_released_without_an_event() {
    let (store, directory) = setup(AGENT_TASK);
    let claim = claim_work(&store, &directory);
    // Reproduce the old commit: the run is cancelled with no claim write.
    let mut work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let step = work.runtime.state.steps[0].id.clone().unwrap();
    work.append(
        runbook_engine::Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        human(1).actor,
    )
    .unwrap();
    work.settle().unwrap();
    assert_eq!(work.runtime.state.steps[0].status, StepStatus::Cancelled);
    let batch = runbook_core::ExecutionCommit {
        execution_id: exe(),
        expected_sequence: work.snapshot.revision,
        events: work.runtime.pending_events().to_vec(),
        projection: runbook_core::start::project_preserving(
            &work.runtime.state,
            work.record.runbook_id.clone(),
            work.record.version_id.clone(),
            None,
        ),
        timers: vec![],
        outbox: vec![],
        idempotency: vec![],
        claims: vec![],
        credentials: vec![],
        derived_outbox: vec![],
    };
    drop(work);
    ready(store.commit(batch)).unwrap();
    assert!(
        !ready(store.claim_record(&exe(), &step))
            .unwrap()
            .unwrap()
            .released
    );

    store.set_time(START_MS + runbook_core::claims::AGENT_LEASE_MS + 1);
    let lease = ready(store.claim_due(10, 60_000))
        .unwrap()
        .into_iter()
        .find(|lease| matches!(lease.id.kind, WorkKind::ClaimLease { .. }))
        .expect("the stranded lease is due");
    let before = ready(store.load(&exe())).unwrap().revision;
    ready(process_lease(&store, &directory, scope(), &lease)).unwrap();
    assert_eq!(ready(store.load(&exe())).unwrap().revision, before);
    assert!(
        ready(store.claim_record(&exe(), &step))
            .unwrap()
            .unwrap()
            .released
    );
    assert!(!lease_offered(&store));
    assert!(
        ready(heartbeat(
            &store,
            &agent(),
            &exe(),
            &step,
            &HeartbeatRequest { claim },
            None
        ))
        .is_err()
    );
    // Expiring again is a no-op.
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    assert!(!ready(expire_claim(work, &step, 1)).unwrap());
}

// F9: PRD §17 per-step limits: outputs 64 KB, logs 5 MB.
#[test]
fn step_outputs_and_logs_are_size_limited() {
    use runbook_core::claims::{
        MAX_STEP_LOG_BYTES, MAX_STEP_OUTPUT_BYTES, append_logs, report_step,
    };
    use runbook_model::{LogsRequest, ReportRequest, ReportStatus};
    let (store, directory) = setup(AGENT_TASK);
    let claim = claim_work(&store, &directory);
    let step = ready(begin_actor(&store, &directory, scope(), &exe()))
        .unwrap()
        .runtime
        .state
        .steps[0]
        .id
        .clone()
        .unwrap();
    let mib = "x".repeat(1024 * 1024);
    assert_eq!(MAX_STEP_LOG_BYTES, 5 * 1024 * 1024);
    for _ in 0..5 {
        let request = LogsRequest {
            claim: claim.clone(),
            logs: vec![mib.clone()],
        };
        ready(append_logs(&store, &agent(), &exe(), &step, &request, None)).unwrap();
    }
    let sixth = LogsRequest {
        claim: claim.clone(),
        logs: vec!["one more line".into()],
    };
    let error = ready(append_logs(&store, &agent(), &exe(), &step, &sixth, None)).unwrap_err();
    assert_eq!(error.code(), "step.logs_too_large");
    // Review M6: a blank line counts too (at least one byte per entry).
    let blank = LogsRequest {
        claim: claim.clone(),
        logs: vec![String::new()],
    };
    let error = ready(append_logs(&store, &agent(), &exe(), &step, &blank, None)).unwrap_err();
    assert_eq!(error.code(), "step.logs_too_large");

    let report = |outputs: Data, logs: Vec<String>| ReportRequest {
        step_run_id: step.clone(),
        claim: claim.clone(),
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs,
        summary: None,
        evidence: vec![],
        logs,
    };
    let big: Data = [(
        "blob".to_string(),
        serde_json::Value::String("x".repeat(MAX_STEP_OUTPUT_BYTES + 1024)),
    )]
    .into();
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let error = ready(report_step(work, &agent(), &report(big, vec![]), None)).unwrap_err();
    assert_eq!(error.code(), "step.outputs_too_large");
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let error = ready(report_step(
        work,
        &agent(),
        &report(Data::new(), vec![mib.clone(); 6]),
        None,
    ))
    .unwrap_err();
    assert_eq!(error.code(), "step.logs_too_large");
    let mut lines = vec![mib.clone(); 5];
    lines.push(String::new());
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let error = ready(report_step(
        work,
        &agent(),
        &report(Data::new(), lines),
        None,
    ))
    .unwrap_err();
    assert_eq!(error.code(), "step.logs_too_large");
    // CF12b review round 2: the budget counts a report's logs as stored,
    // after redaction: a 6 MiB secret is stored as its marker.
    let secret = format!("password={}", "s".repeat(6 * 1024 * 1024));
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(report_step(
        work,
        &agent(),
        &report(Data::new(), vec![secret]),
        None,
    ))
    .unwrap();
}

const AGENT_GATE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: review, title: Review}\n\
    spec:\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Go?\", options: [keep], approvers: {teams: [oncall]}, min: 1, allow_agents: true}\n    \
    - {id: after, title: After}\n";

// F94: an agent's approval decision is an agent decision record (name,
// model, session), not system policy `agent:<name>`, in the decision
// response, the execution document and the resume brief.
#[test]
fn agent_decisions_are_recorded_as_agent_decisions() {
    use runbook_model::{ActorDeclaration, ActorKind, DecisionRequest};
    let (store, directory) = setup(AGENT_GATE);
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let gate = work.runtime.state.steps[0].id.clone().unwrap();
    let principal = ActorPrincipal {
        actor: ActorRef::Agent {
            principal_id: "svc_ops".into(),
            session_id: "s1".into(),
            name: "ops".into(),
            model: Some("model-a".into()),
        },
        ..agent()
    };
    let record = ready(runbook_core::decisions::agent_decide(
        work,
        &principal,
        &gate,
        &DecisionRequest {
            option: "keep".into(),
            comment: Some("checked".into()),
            session_id: Some("s1".into()),
            actor: Some(ActorDeclaration {
                kind: ActorKind::Agent,
                agent_name: Some("ops".into()),
                model: Some("model-a".into()),
            }),
        },
        "s1",
        None,
    ))
    .unwrap();
    let expected = serde_json::json!({
        "by": "agent",
        "agent_name": "ops",
        "model": "model-a",
        "session_id": "s1",
        "option": "keep",
        "comment": "checked",
    });
    assert_eq!(serde_json::to_value(&record).unwrap(), expected);

    let state = ready(begin_actor(&store, &directory, scope(), &exe()))
        .unwrap()
        .runtime
        .state;
    let brief = runbook_core::reads::resume_brief(&state, Some(ActorKind::Agent));
    assert_eq!(
        serde_json::to_value(&brief.decisions).unwrap(),
        serde_json::json!([expected])
    );

    // Records stored before the agent variant existed still read back.
    let legacy: runbook_model::DecisionRecord = serde_json::from_value(
        serde_json::json!({"by": "system", "policy": "agent:ops", "option": "keep"}),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&legacy).unwrap()["policy"],
        "agent:ops"
    );
}

// F15: PRD §6, an approval-expiration pause also stops new claims.
#[test]
fn an_expiration_pause_blocks_new_claims() {
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
        metadata: {id: review, title: Review}\n\
        spec:\n  steps:\n    - id: fan\n      parallel:\n        \
        - id: gate\n          approval: {prompt: \"Go?\", options: [keep], approvers: {teams: [oncall]}, timeout: 1m, escalate_to: [\"team:oncall\"], escalation_timeout: 1m}\n        \
        - {id: work, title: Work, actor: agent}\n";
    let (store, directory) = setup(source);
    store.set_time(START_MS + 120_000);
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(runbook_core::control::tick(work)).unwrap();
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    assert_eq!(work.runtime.state.status, ExecutionStatus::Paused);
    let task = work
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "work")
        .unwrap();
    assert_eq!(task.status, StepStatus::Ready);
    let id = task.id.clone().unwrap();
    let error = ready(claim_step(
        work,
        &agent(),
        &id,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000002".parse().unwrap(),
        None,
    ))
    .unwrap_err();
    assert_eq!(error.code(), "execution.paused");
}

/// LANG9: once a strict task escalates, its escalation recipients may claim
/// it as well as its assignee; before that they may not.
#[test]
fn an_escalation_recipient_may_claim_an_escalated_strict_task() {
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
        metadata: {id: review, title: Review}\n\
        spec:\n  steps:\n    - {id: work, title: Work, actor: human, assignee: user:user1, assignment: strict, escalate_after: 1h, escalate_to: [user:user2]}\n";
    let (store, directory) = setup(source);
    let try_claim = |n: u8, claim: u8| {
        let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
        let id = work.runtime.state.steps[0].id.clone().unwrap();
        ready(claim_step(
            work,
            &human(n),
            &id,
            &ClaimRequest {
                take_over: false,
                session_id: format!("s{claim}"),
                actor: None,
            },
            format!("clm_{claim:026}").parse().unwrap(),
            None,
        ))
        .map_err(|e| e.code())
    };
    assert_eq!(try_claim(2, 1).unwrap_err(), "claim.not_assignee");
    store.set_time(START_MS + 3_600_000);
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(runbook_core::control::tick(work)).unwrap();
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let escalation = work.runtime.state.steps[0].followup_escalation.clone();
    drop(work);
    assert_eq!(
        escalation.unwrap().recipients,
        [runbook_model::RecipientRef::User(usr(2))]
    );
    try_claim(2, 2).unwrap();
}

// F16: a run that cannot quiesce in one settle (a set-only loop with no
// delay) fails with a recorded fault instead of wedging every commit.
#[test]
fn a_run_that_cannot_quiesce_fails_instead_of_wedging() {
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
        metadata: {id: review, title: Review}\n\
        spec:\n  vars: {n: 0}\n  steps:\n    \
        - id: spin\n      loop:\n        max_iterations: 50\n        until: '${{ vars.n < 0 }}'\n        \
        steps:\n          - id: inner\n            loop:\n              max_iterations: 50\n              until: '${{ vars.n < 0 }}'\n              \
        steps: [{id: bump, set: {vars: {n: '${{ iteration }}'}}}]\n";
    let (store, directory) = setup(source);
    let state = ready(begin_actor(&store, &directory, scope(), &exe()))
        .unwrap()
        .runtime
        .state;
    assert_eq!(state.status, ExecutionStatus::Failed);
    assert_eq!(
        state.terminal_reason.as_deref(),
        Some("engine.failed_to_quiesce")
    );
}

// F21: auto-disable needs continuous failure. Two failures three days
// apart with no attempt between them start a fresh streak instead.
#[test]
fn webhook_auto_disable_needs_continuous_failure() {
    use runbook_core::webhooks::{DISABLE_AFTER_MS, RETRY_WINDOW_MS, Subscription};
    let fresh = || -> Subscription {
        serde_json::from_value(serde_json::json!({
            "id": "whk_00000000000000000000000001",
            "scope": {"org_id": scope().org_id, "project_id": scope().project_id},
            "name": "hook", "url": "https://hooks.example/1", "events": ["*"],
            "runbooks": [], "tags": [], "enabled": true, "secret": "sealed",
            "previous_secret": null, "created_at_ms": 0, "updated_at_ms": 0,
            "secret_rotated_at_ms": 0, "failing_since_ms": null,
            "disabled_reason": null, "disabled_at_ms": null
        }))
        .unwrap()
    };
    let mut sparse = fresh();
    sparse.record_attempt(false, 0, Some("timeout"));
    sparse.record_attempt(false, DISABLE_AFTER_MS, Some("timeout"));
    assert!(sparse.enabled, "two isolated failures are not a streak");
    assert_eq!(sparse.failing_since_ms, Some(DISABLE_AFTER_MS));

    let mut continuous = fresh();
    let mut at = 0;
    while at < DISABLE_AFTER_MS {
        continuous.record_attempt(false, at, Some("timeout"));
        at += RETRY_WINDOW_MS / 2;
    }
    continuous.record_attempt(false, DISABLE_AFTER_MS, Some("timeout"));
    assert!(!continuous.enabled);
}
