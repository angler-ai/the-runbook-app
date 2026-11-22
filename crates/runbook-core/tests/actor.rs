//! B6a memory-side actor semantics under a fake clock: vote deadline
//! equality in both orderings, and resume clearing only the manual pause.
use runbook_core::{
    actor::{ActorPrincipal, begin_actor},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    control::{set_paused, tick},
    decisions::vote,
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, TeamRecord, UserRecord,
        memory::MemoryDirectory,
    },
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, activate, prepare_start_commit},
    work_queue::WorkQueue,
};
use runbook_model::{ActorRef, Data, DecisionRequest, ExecutionId, ProjectRole, RunbookId, UserId};
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
const GATE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gate-check, title: Gate check}\n\
    spec:\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Go?\", options: [keep], approvers: {teams: [oncall]}, min: 1, timeout: 1m, on_timeout: escalate, escalate_to: [\"team:oncall\"], escalation_timeout: 1m}\n    \
    - id: after\n      title: After\n";
const GATE_PLAIN: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gate-check, title: Gate check}\n\
    spec:\n  execution: {timeout: 1h}\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Go?\", options: [keep], approvers: {teams: [oncall]}, min: 1}\n    \
    - id: after\n      title: After\n";

fn setup(source: &str) -> (MemoryExecutionStore, MemoryDirectory) {
    let projection = runbook_core::ExecutionProjection {
        execution_id: "exe_00000000000000000000000090".parse().unwrap(),
        runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
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
            slug: "gate-check".into(),
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
        ..Default::default()
    };
    let prepared = prepare(
        "gate-check",
        source,
        &context,
        "rbv_00000000000000000000000001".parse().unwrap(),
    )
    .unwrap();
    ready(publish_draft(&store, &store, &rb, 1, prepared)).unwrap();
    let command = StartCommand {
        execution_id: "exe_00000000000000000000000001".parse().unwrap(),
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
fn exe() -> ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}

#[test]
fn vote_at_execution_deadline_equality_is_rejected_both_orderings() {
    // Request-first: the session clock sits exactly on the deadline.
    let (store, directory) = setup(GATE_PLAIN);
    store.set_time(1_000 + 3_600_000);
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let gate = w
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "gate")
        .and_then(|s| s.id.clone())
        .unwrap();
    let rejected = ready(vote(
        w,
        &human(1),
        &gate,
        &DecisionRequest {
            option: "keep".into(),
            comment: None,
            session_id: None,
            actor: None,
        },
        None,
    ));
    assert!(rejected.is_err(), "vote at deadline equality must reject");
    // Timer-first: ticking to the deadline closes the run, then votes fail.
    let (store, directory) = setup(GATE_PLAIN);
    store.set_time(1_000 + 3_600_000);
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(tick(w)).unwrap();
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let gate = w
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "gate")
        .and_then(|s| s.id.clone())
        .unwrap();
    assert!(
        ready(vote(
            w,
            &human(1),
            &gate,
            &DecisionRequest {
                option: "keep".into(),
                comment: None,
                session_id: None,
                actor: None,
            },
            None,
        ))
        .is_err()
    );
}

#[test]
fn resume_clears_only_manual_pause() {
    let (store, directory) = setup(GATE);
    // Escalate, then expire: the approval-expired pause reason appears.
    store.set_time(1_000 + 61_000);
    ready(tick(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
    ))
    .unwrap();
    store.set_time(1_000 + 122_000);
    ready(tick(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
    ))
    .unwrap();
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    assert!(
        w.runtime
            .state
            .pause_reasons
            .iter()
            .any(|reason| matches!(reason, runbook_engine::PauseReason::ApprovalExpired(_))),
        "{:?}",
        w.runtime.state.pause_reasons
    );
    drop(w);
    ready(set_paused(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &human(1),
        true,
        None,
    ))
    .unwrap();
    ready(set_paused(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &human(1),
        false,
        None,
    ))
    .unwrap();
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    assert!(
        !w.runtime
            .state
            .pause_reasons
            .contains(&runbook_engine::PauseReason::Manual)
    );
    assert!(
        w.runtime
            .state
            .pause_reasons
            .iter()
            .any(|reason| matches!(reason, runbook_engine::PauseReason::ApprovalExpired(_))),
        "resume must not clear the expired-approval pause"
    );
    drop(w);
}

#[test]
fn actor_replay_returns_saved_vote_before_stale_gate_checks() {
    use runbook_core::{ExecutionStore, actor::Replay};
    let (store, directory) = setup(GATE_PLAIN);
    let principal = human(1);
    let replay = Replay {
        key: "vote-once".into(),
        principal: principal.principal.clone(),
        request_hash: [9; 32],
    };
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let step = work
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "gate")
        .unwrap()
        .id
        .clone()
        .unwrap();
    let request = DecisionRequest {
        option: "keep".into(),
        comment: None,
        session_id: None,
        actor: None,
    };
    ready(vote(work, &principal, &step, &request, Some(&replay))).unwrap();
    let revision = ready(store.load(&exe())).unwrap().revision;
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(vote(work, &principal, &step, &request, Some(&replay))).unwrap();
    assert_eq!(ready(store.load(&exe())).unwrap().revision, revision);
    let mut changed = replay.clone();
    changed.request_hash = [8; 32];
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    assert_eq!(
        ready(vote(work, &principal, &step, &request, Some(&changed)))
            .unwrap_err()
            .code(),
        "request.idempotency_key_reused"
    );
    store.set_time(1_000 + 86_400_000);
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    assert!(
        ready(vote(work, &principal, &step, &request, Some(&replay))).is_err(),
        "expired replay must recheck the now-closed gate"
    );
}

#[test]
fn no_op_replay_commit_cannot_change_projection() {
    use runbook_core::{ExecutionStore, actor::Replay};
    let (store, directory) = setup(GATE_PLAIN);
    let principal = human(1);
    let replay = Replay {
        key: "resume".into(),
        principal: principal.principal.clone(),
        request_hash: [3; 32],
    };
    let before = ready(store.load(&exe())).unwrap();
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(set_paused(work, &principal, false, Some(&replay))).unwrap();
    let after = ready(store.load(&exe())).unwrap();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.projection, before.projection);
    assert!(after.idempotency.is_empty());
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(set_paused(work, &principal, false, Some(&replay))).unwrap();
    let mut projection = after.projection;
    projection
        .outputs
        .insert("tampered".into(), serde_json::json!(true));
    let mut row = replay.record("set_paused", &exe(), 1_000, ()).unwrap();
    row.key.push_str("-tampered");
    let batch = runbook_core::ExecutionCommit {
        execution_id: exe(),
        expected_sequence: after.revision,
        events: vec![],
        projection,
        timers: vec![],
        outbox: vec![],
        idempotency: vec![row],
        claims: vec![],
        credentials: vec![],
        derived_outbox: vec![],
    };
    assert_eq!(
        ready(store.commit(batch)).unwrap_err(),
        runbook_core::StoreError::InvalidBatch("store.replay_projection_changed")
    );
}

#[test]
fn lease_and_log_replay_survive_claim_release_in_memory() {
    use runbook_core::{
        WriteStage,
        actor::Replay,
        claims::{append_logs, claim_step, heartbeat, report_step},
    };
    use runbook_model::{ClaimRequest, HeartbeatRequest, LogsRequest, ReportRequest, ReportStatus};
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: gate-check, title: Tasks}\nspec:\n  steps: [{id: first, title: First, actor: any}, {id: second, title: Second}]\n";
    let (store, directory) = setup(source);
    let principal = human(1);
    let replay = |key: &str| Replay {
        key: key.into(),
        principal: principal.principal.clone(),
        request_hash: [1; 32],
    };
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let step = work.runtime.state.steps[0].id.clone().unwrap();
    let claim = ready(claim_step(
        work,
        &principal,
        &step,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        Some(&replay("claim")),
    ))
    .unwrap();
    let heart = HeartbeatRequest {
        claim: claim.clone(),
    };
    assert_eq!(
        ready(heartbeat(
            &store,
            &principal,
            &exe(),
            &step,
            &heart,
            Some(&replay("heart"))
        ))
        .unwrap(),
        None
    );
    let logs = LogsRequest {
        claim: claim.clone(),
        logs: vec!["once".into()],
    };
    store.fail_after(WriteStage::Idempotency);
    assert!(
        ready(append_logs(
            &store,
            &principal,
            &exe(),
            &step,
            &logs,
            Some(&replay("logs"))
        ))
        .is_err()
    );
    assert_eq!(
        ready(append_logs(
            &store,
            &principal,
            &exe(),
            &step,
            &logs,
            Some(&replay("logs"))
        ))
        .unwrap(),
        1
    );
    // F46: the line reads back by sequence.
    assert_eq!(
        ready(runbook_core::claims::ClaimStore::read_logs(
            &store,
            &exe(),
            &step,
            0,
            10
        ))
        .unwrap(),
        vec![(1, "once".to_owned())]
    );
    assert!(
        ready(runbook_core::claims::ClaimStore::read_logs(
            &store,
            &exe(),
            &step,
            1,
            10
        ))
        .unwrap()
        .is_empty()
    );
    let report = ReportRequest {
        step_run_id: step.clone(),
        claim,
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Default::default(),
        summary: None,
        evidence: vec![],
        logs: vec![],
        cost: None,
    };
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(report_step(
        work,
        &principal,
        &report,
        Some(&replay("report")),
    ))
    .unwrap();
    assert_eq!(
        ready(append_logs(
            &store,
            &principal,
            &exe(),
            &step,
            &logs,
            Some(&replay("logs"))
        ))
        .unwrap(),
        1
    );
    assert_eq!(
        ready(heartbeat(
            &store,
            &principal,
            &exe(),
            &step,
            &heart,
            Some(&replay("heart"))
        ))
        .unwrap(),
        None
    );
    assert!(ready(heartbeat(&store, &principal, &exe(), &step, &heart, None)).is_err());
}

#[test]
fn terminal_no_op_replay_preserves_completion_time() {
    use runbook_core::{
        ExecutionStore,
        actor::Replay,
        claims::{claim_step, report_step},
        control::cancel,
    };
    use runbook_model::{ClaimRequest, ReportRequest, ReportStatus};
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: gate-check, title: Task}\nspec:\n  steps: [{id: first, title: First, actor: any}]\n";
    let (store, directory) = setup(source);
    let principal = human(1);
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let step = work.runtime.state.steps[0].id.clone().unwrap();
    let claim = ready(claim_step(
        work,
        &principal,
        &step,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    ))
    .unwrap();
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(report_step(
        work,
        &principal,
        &ReportRequest {
            step_run_id: step,
            claim,
            status: ReportStatus::Succeeded,
            verdict: None,
            outputs: Default::default(),
            summary: None,
            evidence: vec![],
            logs: vec![],
            cost: None,
        },
        None,
    ))
    .unwrap();
    let before = ready(store.load(&exe())).unwrap();
    assert_eq!(
        before.projection.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    store.set_time(5_000);
    let replay = Replay {
        key: "cancel-finished".into(),
        principal: principal.principal.clone(),
        request_hash: [1; 32],
    };
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(cancel(&store, work, &principal, Some(&replay))).unwrap();
    let after = ready(store.load(&exe())).unwrap();
    assert_eq!(after.projection, before.projection);
    assert_eq!(after.revision, before.revision);
    assert!(after.idempotency.is_empty());
}

#[test]
fn no_op_replay_does_not_rewrite_timer_generations() {
    use runbook_core::{ExecutionStore, TimerChange, actor::Replay};
    let (store, directory) = setup(GATE_PLAIN);
    let before = ready(store.load(&exe())).unwrap();
    let mut timer = before.timers.values().next().unwrap().clone();
    timer.generation += 1;
    let mut projection = before.projection;
    projection.sequence += 1;
    ready(store.commit(runbook_core::ExecutionCommit {
        execution_id: exe(),
        expected_sequence: before.revision,
        events: vec![runbook_engine::EventEnvelope {
            execution_id: exe(),
            seq: projection.sequence,
            at_ms: 1_000,
            actor: ActorRef::System,
            event: runbook_engine::Event::ManualPauseChanged { paused: false },
        }],
        projection,
        timers: vec![TimerChange::Upsert(timer.clone())],
        outbox: vec![],
        idempotency: vec![],
        claims: vec![],
        credentials: vec![],
        derived_outbox: vec![],
    }))
    .unwrap();
    let principal = human(1);
    let replay = Replay {
        key: "noop-timer".into(),
        principal: principal.principal.clone(),
        request_hash: [1; 32],
    };
    let work = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(set_paused(work, &principal, false, Some(&replay))).unwrap();
    assert_eq!(ready(store.load(&exe())).unwrap().timers[&timer.key], timer);
}

#[test]
fn step_run_lookup_is_org_scoped() {
    use runbook_core::step_lookup::StepRunLookup;
    let (store, directory) = setup(GATE_PLAIN);
    let exe: ExecutionId = "exe_00000000000000000000000001".parse().unwrap();
    let work = ready(begin_actor(&store, &directory, scope(), &exe)).unwrap();
    let sr = work
        .runtime
        .state
        .steps
        .iter()
        .find_map(|s| s.id.clone())
        .unwrap();
    drop(work);
    assert_eq!(
        ready(store.step_run_execution(&scope().org_id, &sr)).unwrap(),
        Some((exe.clone(), scope().project_id))
    );
    let command = StartCommand {
        execution_id: "exe_00000000000000000000000002".parse().unwrap(),
        runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: "lookup-second".into(),
        principal: "user:alice".into(),
        request_hash: [2; 32],
    };
    let commit = ready(prepare_start_commit(&store, &store, &command)).unwrap();
    ready(store.commit_start(commit)).unwrap();
    let lease = ready(store.claim_due(10, 60_000))
        .unwrap()
        .into_iter()
        .find(|l| {
            l.id.execution_id == command.execution_id
                && l.id.kind == runbook_core::work_queue::WorkKind::Activation
        })
        .unwrap();
    ready(activate(&store, &directory, scope(), &lease)).unwrap();
    let second = ready(begin_actor(
        &store,
        &directory,
        scope(),
        &command.execution_id,
    ))
    .unwrap();
    let second_sr = second
        .runtime
        .state
        .steps
        .iter()
        .find_map(|s| s.id.clone())
        .unwrap();
    drop(second);
    assert_ne!(
        sr, second_sr,
        "step URLs must identify exactly one execution"
    );
    assert_eq!(
        ready(store.step_run_execution(&scope().org_id, &second_sr)).unwrap(),
        Some((command.execution_id, scope().project_id))
    );
    let other = "org_00000000000000000000000002".parse().unwrap();
    assert_eq!(ready(store.step_run_execution(&other, &sr)).unwrap(), None);
    let missing = "sr_00000000000000000000000099".parse().unwrap();
    assert_eq!(
        ready(store.step_run_execution(&scope().org_id, &missing)).unwrap(),
        None
    );
}

#[test]
fn metadata_and_control_commit_together() {
    use runbook_core::{ExecutionStore, control::patch};
    use runbook_model::{ExecutionAction, ExecutionPatchRequest};
    let source = GATE_PLAIN.replace("  steps:", "  fields:\n    severity: {type: integer, default: 1, indexed: true}\n    fixed: {type: string, default: pinned, locked: true}\n  steps:");
    let (store, directory) = setup(&source);
    let request = ExecutionPatchRequest {
        action: Some(ExecutionAction::Pause),
        title: Some("Incident".into()),
        tags: Some(vec!["urgent".into()]),
        fields: Some([("severity".into(), 3.into())].into()),
    };
    let before = ready(store.load(&exe())).unwrap();
    store.fail_after(runbook_core::WriteStage::DerivedRows);
    assert!(
        ready(patch(
            ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
            &human(1),
            &request,
            None
        ))
        .is_err()
    );
    assert_eq!(ready(store.load(&exe())).unwrap().revision, before.revision);
    ready(patch(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &human(1),
        &request,
        None,
    ))
    .unwrap();
    let summary = ready(store.get_run(&exe())).unwrap().unwrap();
    assert_eq!(summary.title, "Incident");
    assert_eq!(summary.tags, ["urgent"]);
    assert_eq!(summary.fields["severity"], 3);
    assert_eq!(summary.status, runbook_model::ExecutionStatus::Paused);
    let revision = ready(store.load(&exe())).unwrap().revision;
    ready(patch(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &human(1),
        &request,
        None,
    ))
    .unwrap();
    assert_eq!(
        ready(store.load(&exe())).unwrap().revision,
        revision,
        "PATCH retry is idempotent"
    );
    let invalid = ExecutionPatchRequest {
        fields: Some([("fixed".into(), "changed".into())].into()),
        ..request
    };
    assert!(
        ready(patch(
            ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
            &human(1),
            &invalid,
            None
        ))
        .is_err()
    );
    assert_eq!(ready(store.load(&exe())).unwrap().revision, revision);
}

#[test]
fn queued_cancel_is_terminal_atomic_and_never_activated() {
    use runbook_core::{ExecutionStore, control::patch};
    use runbook_model::{ExecutionAction, ExecutionPatchRequest};
    let source = GATE_PLAIN.replace(
        "execution: {timeout: 1h}",
        "execution: {timeout: 1h, concurrency: {key: api, limit: 1}}",
    );
    let (store, directory) = setup(&source);
    let command = StartCommand {
        execution_id: "exe_00000000000000000000000002".parse().unwrap(),
        runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: Some("Waiting".into()),
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: "queued-2".into(),
        principal: "user:alice".into(),
        request_hash: [2; 32],
    };
    ready(store.commit_start(ready(prepare_start_commit(&store, &store, &command)).unwrap()))
        .unwrap();
    let id = &command.execution_id;
    let request = ExecutionPatchRequest {
        action: Some(ExecutionAction::Cancel),
        title: None,
        tags: None,
        fields: None,
    };
    for rejected in [
        ExecutionPatchRequest {
            action: Some(ExecutionAction::Resume),
            ..request.clone()
        },
        ExecutionPatchRequest {
            title: Some("No".into()),
            ..request.clone()
        },
    ] {
        let error = ready(patch(
            ready(begin_actor(&store, &directory, scope(), id)).unwrap(),
            &human(1),
            &rejected,
            None,
        ))
        .unwrap_err();
        assert_eq!(error.code(), "execution.not_started");
    }
    store.fail_after(runbook_core::WriteStage::DerivedRows);
    assert!(
        ready(patch(
            ready(begin_actor(&store, &directory, scope(), id)).unwrap(),
            &human(1),
            &request,
            None
        ))
        .is_err()
    );
    assert_eq!(
        ready(store.admission(id)).unwrap().unwrap().state,
        runbook_core::catalog::AdmissionState::Queued
    );
    ready(patch(
        ready(begin_actor(&store, &directory, scope(), id)).unwrap(),
        &human(1),
        &request,
        None,
    ))
    .unwrap();
    let cancelled = ready(store.load(id)).unwrap();
    assert_eq!(
        cancelled.projection.status,
        runbook_model::ExecutionStatus::Cancelled
    );
    assert!(cancelled.projection.steps.is_empty());
    assert_eq!(ready(store.get_run(id)).unwrap().unwrap().title, "Waiting");
    assert_eq!(
        ready(store.admission(id)).unwrap().unwrap().state,
        runbook_core::catalog::AdmissionState::Released
    );
    ready(runbook_core::control::cancel(
        &store,
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &human(1),
        None,
    ))
    .unwrap();
    assert!(
        ready(store.claim_due(100, 60_000))
            .unwrap()
            .iter()
            .all(|l| l.id.execution_id != *id
                || l.id.kind != runbook_core::work_queue::WorkKind::Activation)
    );
    ready(patch(
        ready(begin_actor(&store, &directory, scope(), id)).unwrap(),
        &human(1),
        &request,
        None,
    ))
    .unwrap();
    assert_eq!(ready(store.load(id)).unwrap().revision, cancelled.revision);
    assert_eq!(
        ready(store.admission(id)).unwrap().unwrap().state,
        runbook_core::catalog::AdmissionState::Released
    );
    let metadata = ExecutionPatchRequest {
        action: None,
        title: Some("After".into()),
        tags: None,
        fields: None,
    };
    assert_eq!(
        ready(patch(
            ready(begin_actor(&store, &directory, scope(), id)).unwrap(),
            &human(1),
            &metadata,
            None
        ))
        .unwrap_err()
        .code(),
        "execution.closed"
    );
}

#[test]
fn actor_replay_storage_does_not_grow_session_snapshots() {
    use runbook_core::{
        ExecutionStore,
        actor::Replay,
        claims::{claim_step, heartbeat},
    };
    use runbook_model::{ClaimRequest, HeartbeatRequest};
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: gate-check, title: Check}\nspec:\n  steps: [{id: work, title: Work, actor: human}]\n";
    let (store, directory) = setup(source);
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let sr = w
        .runtime
        .state
        .steps
        .iter()
        .find_map(|s| s.id.clone())
        .unwrap();
    let claim = ready(claim_step(
        w,
        &human(1),
        &sr,
        &ClaimRequest {
            take_over: false,
            session_id: "web".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    ))
    .unwrap();
    for n in 0..50 {
        let replay = Replay {
            key: format!("beat-{n}"),
            principal: human(1).principal,
            request_hash: [1; 32],
        };
        ready(heartbeat(
            &store,
            &human(1),
            &exe(),
            &sr,
            &HeartbeatRequest {
                claim: claim.clone(),
            },
            Some(&replay),
        ))
        .unwrap();
    }
    assert!(
        ready(store.load(&exe())).unwrap().idempotency.is_empty(),
        "actor replay rows must stay out of session snapshots"
    );
    let replay = Replay {
        key: "beat-49".into(),
        principal: human(1).principal,
        request_hash: [1; 32],
    };
    let operation = format!("heartbeat:{sr}");
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    assert_eq!(
        ready(w.replayed::<Option<i64>>(&human(1), Some(&replay), &operation)).unwrap(),
        Some(None)
    );
    drop(w);
    store.set_time(1_000 + 86_400_000);
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    assert_eq!(
        ready(w.replayed::<Option<i64>>(&human(1), Some(&replay), &operation)).unwrap(),
        None
    );
    drop(w);
    ready(heartbeat(
        &store,
        &human(1),
        &exe(),
        &sr,
        &HeartbeatRequest { claim },
        Some(&replay),
    ))
    .unwrap();
    ready(runbook_core::control::cancel(
        &store,
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &human(1),
        None,
    ))
    .unwrap();
    use runbook_core::{retention::RunStore, step_lookup::StepRunLookup};
    assert_eq!(
        ready(store.export_execution(&exe()))
            .unwrap()
            .actor_replays
            .len(),
        1,
        "expired rows purged on insert"
    );
    ready(store.delete_execution(&exe(), "test purge")).unwrap();
    assert_eq!(
        ready(store.step_run_execution(&scope().org_id, &sr)).unwrap(),
        None
    );
}

#[test]
fn replay_timestamp_is_fenced_by_session_time() {
    use runbook_core::{
        ExecutionStore,
        actor::Replay,
        session::{ExecutionSession, SessionStore},
    };
    let (store, _) = setup(GATE_PLAIN);
    let session = ready(store.begin(&exe())).unwrap();
    let snapshot = ready(session.load(&exe())).unwrap();
    let replay = Replay {
        key: "future".into(),
        principal: human(1).principal,
        request_hash: [1; 32],
    };
    let batch = runbook_core::ExecutionCommit {
        execution_id: exe(),
        expected_sequence: snapshot.revision,
        projection: snapshot.projection,
        events: vec![],
        timers: vec![],
        outbox: vec![],
        idempotency: vec![
            replay
                .record(
                    "future",
                    &exe(),
                    session.transaction_time_ms() + 86_400_000,
                    (),
                )
                .unwrap(),
        ],
        claims: vec![],
        credentials: vec![],
        derived_outbox: vec![],
    };
    assert_eq!(
        ready(session.commit(batch)).unwrap_err(),
        runbook_core::StoreError::InvalidBatch("store.transaction_time_mismatch")
    );
}

#[test]
fn admin_release_advances_generation_in_memory() {
    use runbook_core::{
        ExecutionStore,
        claims::{admin_release, claim_step, report_step},
    };
    use runbook_model::{ClaimRequest, ReportRequest, ReportStatus};
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: gate-check, title: Check}\nspec:\n  steps: [{id: work, title: Work, actor: human}]\n";
    let (store, directory) = setup(source);
    let revision = ready(directory.snapshot(&scope().org_id)).unwrap().revision;
    ready(directory.commit(DirectoryCommit {
        org_id: scope().org_id,
        expected_revision: revision,
        mutations: vec![DirectoryMutation::GrantProjectRole {
            project_id: scope().project_id,
            user_id: usr(2),
            role: ProjectRole::Admin,
        }],
    }))
    .unwrap();
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let sr = w
        .runtime
        .state
        .steps
        .iter()
        .find_map(|s| s.id.clone())
        .unwrap();
    let claim = ready(claim_step(
        w,
        &human(1),
        &sr,
        &ClaimRequest {
            take_over: false,
            session_id: "web".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    ))
    .unwrap();
    ready(admin_release(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &human(2),
        &sr,
        "reassign",
        None,
    ))
    .unwrap();
    let snapshot = ready(store.load(&exe())).unwrap();
    assert_eq!(snapshot.claims[&sr].generation, claim.generation.get() + 1);
    assert!(snapshot.events.iter().any(|e| e.actor==human(2).actor && matches!(&e.event,runbook_engine::Event::StepClaimReleased{reason:Some(reason),..} if reason=="reassign")));
    let report = ReportRequest {
        step_run_id: sr,
        claim,
        status: ReportStatus::Succeeded,
        verdict: None,
        outputs: Data::new(),
        summary: None,
        evidence: vec![],
        logs: vec![],
        cost: None,
    };
    assert_eq!(
        ready(report_step(
            ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
            &human(1),
            &report,
            None
        ))
        .unwrap_err()
        .code(),
        "claim.stale_generation"
    );
}
