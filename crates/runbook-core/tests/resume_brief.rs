//! B8a `fresh_session_brief_is_sufficient` (core part): a fresh session
//! resumes from the brief alone: completed outputs and summaries, approval
//! decisions and comments, ready references for its audience and the next
//! action, and never the instructions of steps that are not ready yet.
use runbook_core::{
    actor::{ActorPrincipal, begin_actor},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    claims::{claim_step, report_step},
    control::set_paused,
    decisions::vote,
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, TeamRecord, UserRecord,
        memory::MemoryDirectory,
    },
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    reads::resume_brief,
    start::{StartCommand, StartStore, activate, prepare_start_commit},
    work_queue::WorkQueue,
};
use runbook_engine::ExecutionState;
use runbook_model::{
    ActorKind, ActorRef, ClaimRequest, Data, DecisionRecord, DecisionRequest, ExecutionId,
    NextAction, ProjectRole, ReportRequest, ReportStatus, RunbookId, TeamId, UserId,
};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

const FUTURE: &str = "FUTURE-ONLY roll back the deploy";
const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gate-check, title: Gate check}\n\
    spec:\n  defaults: {actor: agent}\n  steps:\n    \
    - id: probe\n      title: Probe\n      outputs: {rate: {type: number, required: true}}\n    \
    - id: gate\n      approval: {prompt: \"Go?\", options: [keep], approvers: {teams: [oncall]}, min: 1}\n    \
    - id: apply\n      title: Apply\n      instructions: FUTURE-ONLY roll back the deploy\n    \
    - id: confirm\n      title: Confirm\n      actor: human\n";

fn agent() -> ActorPrincipal {
    ActorPrincipal {
        principal: "service:runner".into(),
        actor: ActorRef::Agent {
            principal_id: "runner".into(),
            session_id: "s1".into(),
            name: "bot".into(),
            model: None,
        },
        credential_ref: "svc-1".into(),
    }
}
fn state(store: &MemoryExecutionStore, directory: &MemoryDirectory) -> ExecutionState {
    ready(begin_actor(store, directory, scope(), &exe()))
        .unwrap()
        .runtime
        .state
}
fn step_id(state: &ExecutionState, step: &str) -> runbook_model::StepRunId {
    state
        .steps
        .iter()
        .find(|s| s.step_id == step)
        .and_then(|s| s.id.clone())
        .unwrap()
}

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
fn team(n: u8) -> TeamId {
    format!("team_{n:026}").parse().unwrap()
}
fn exe() -> ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}

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
    ];
    for (n, slug) in [(1u8, "oncall"), (2, "leads")] {
        mutations.push(DirectoryMutation::CreateTeam(TeamRecord {
            id: team(n),
            org_id: scope().org_id,
            slug: slug.into(),
            title: slug.into(),
        }));
    }
    for (n, team_n) in [(1u8, 1u8), (2, 1), (3, 2)] {
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
            team_id: team(team_n),
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
        teams: [("oncall".into(), team(1)), ("leads".into(), team(2))].into(),
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

#[test]
fn fresh_session_brief_is_sufficient() {
    let (store, directory) = setup(SPEC);

    // Start: the agent is told to claim the probe; a human has nothing yet.
    let brief = resume_brief(&state(&store, &directory), Some(ActorKind::Agent));
    assert_eq!(brief.next_action, NextAction::ClaimReadyWork);
    assert_eq!(brief.ready_work.len(), 1);
    assert_eq!(brief.ready_work[0].path, "probe");
    assert_eq!(brief.ready_work[0].actor_kind, ActorKind::Agent);
    let human = resume_brief(&state(&store, &directory), Some(ActorKind::Human));
    assert!(human.ready_work.is_empty());
    assert_eq!(human.next_action, NextAction::Wait);

    // The agent completes the probe, then its session exits at the gate.
    let probe = step_id(&state(&store, &directory), "probe");
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let claim = ready(claim_step(
        w,
        &agent(),
        &probe,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    ))
    .unwrap();
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(report_step(
        w,
        &agent(),
        &ReportRequest {
            step_run_id: probe.clone(),
            claim,
            status: ReportStatus::Succeeded,
            verdict: None,
            outputs: Data::from([("rate".to_string(), serde_json::json!(0.01))]),
            summary: Some("error rate is low".into()),
            evidence: vec![],
            logs: vec![],
            cost: None,
        },
        None,
    ))
    .unwrap();
    let waiting = resume_brief(&state(&store, &directory), Some(ActorKind::Agent));
    assert_eq!(waiting.next_action, NextAction::Wait);
    assert!(waiting.ready_work.is_empty());
    assert_eq!(waiting.completed_steps.len(), 1);
    assert_eq!(
        waiting.completed_steps[0].outputs["rate"],
        serde_json::json!(0.01)
    );
    assert_eq!(
        waiting.completed_steps[0].summary.as_deref(),
        Some("error rate is low")
    );

    // A human approves with a comment; a fresh agent session resumes from
    // the brief alone (a new begin_actor read, as after a restart).
    let gate = step_id(&state(&store, &directory), "gate");
    ready(vote(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &ActorPrincipal {
            principal: format!("user:{}", usr(1)),
            actor: ActorRef::Human { user_id: usr(1) },
            credential_ref: "pat".into(),
        },
        &gate,
        &DecisionRequest {
            option: "keep".into(),
            comment: Some("ship it".into()),
            session_id: None,
            actor: None,
        },
        None,
    ))
    .unwrap();
    let resumed = resume_brief(&state(&store, &directory), Some(ActorKind::Agent));
    assert_eq!(resumed.next_action, NextAction::ClaimReadyWork);
    assert_eq!(
        resumed
            .ready_work
            .iter()
            .map(|w| w.path.as_str())
            .collect::<Vec<_>>(),
        vec!["apply"]
    );
    assert!(matches!(
        resumed.decisions.as_slice(),
        [DecisionRecord::Human { user_id, option, comment: Some(c), .. }]
            if user_id == &usr(1) && option == "keep" && c == "ship it"
    ));
    assert_eq!(resumed.comments.len(), 1);
    assert_eq!(resumed.comments[0].body, "ship it");
    assert_eq!(resumed.completed_steps[0].path, "probe");
    // References only: no instructions of any step, ready or future.
    let json = serde_json::to_string(&resumed).unwrap();
    assert!(!json.contains(FUTURE), "{json}");
    // The human-only confirm step is not offered to the agent.
    assert!(
        resumed
            .ready_work
            .iter()
            .all(|w| w.actor_kind == ActorKind::Agent)
    );
}

const AGENT_GATE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gate-check, title: Gate check}\n\
    spec:\n  defaults: {actor: agent}\n  steps:\n    \
    - id: gate\n      title: Gate\n      approval: {prompt: \"Go?\", options: [keep], approvers: {teams: [oncall]}, min: 1, allow_agents: true}\n    \
    - id: after\n      title: After\n";

#[test]
fn agent_decidable_approval_is_ready_for_agents_only() {
    let (store, directory) = setup(AGENT_GATE);
    let agent = resume_brief(&state(&store, &directory), Some(ActorKind::Agent));
    assert_eq!(agent.next_action, NextAction::ClaimReadyWork);
    assert_eq!(
        agent
            .ready_work
            .iter()
            .map(|w| w.path.as_str())
            .collect::<Vec<_>>(),
        vec!["gate"]
    );
    assert_eq!(agent.ready_work[0].actor_kind, ActorKind::Agent);
    let human = resume_brief(&state(&store, &directory), Some(ActorKind::Human));
    assert!(human.ready_work.is_empty(), "{:?}", human.ready_work);
}

#[test]
fn paused_run_offers_no_ready_work() {
    let (store, directory) = setup(SPEC);
    let principal = ActorPrincipal {
        principal: format!("user:{}", usr(1)),
        actor: ActorRef::Human { user_id: usr(1) },
        credential_ref: "pat".into(),
    };
    ready(set_paused(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &principal,
        true,
        None,
    ))
    .unwrap();
    let brief = resume_brief(&state(&store, &directory), Some(ActorKind::Agent));
    assert!(brief.ready_work.is_empty(), "{:?}", brief.ready_work);
    assert_eq!(brief.next_action, NextAction::Wait);
}

const FOREACH: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gate-check, title: Gate check}\n\
    spec:\n  defaults: {actor: agent}\n  \
    inputs: {regions: {type: list, items: {type: string}, default: [east, west]}}\n  steps:\n    \
    - id: regions\n      foreach:\n        items: \"${{ inputs.regions }}\"\n        as: region\n        concurrency: 2\n        max_items: 4\n        \
    steps: [{id: probe, title: Probe}]\n";

/// C4: an iteration's step run path is its engine path (PRD §4:
/// `regions[0].probe`, `stabilize#2.recheck`), never prefixed twice.
#[test]
fn iteration_paths_are_not_prefixed_twice() {
    let (store, directory) = setup(FOREACH);
    let brief = resume_brief(&state(&store, &directory), Some(ActorKind::Agent));
    assert_eq!(
        brief
            .ready_work
            .iter()
            .map(|w| w.path.as_str())
            .collect::<Vec<_>>(),
        vec!["regions[0].probe", "regions[1].probe"]
    );
}

const HANDOFF: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gate-check, title: Gate check, owners: [team:oncall]}\n\
    spec:\n  defaults: {actor: agent, on_failure: handoff}\n  steps:\n    \
    - {id: probe, title: Probe}\n    - {id: after, title: After}\n";

fn human(n: u8) -> ActorPrincipal {
    ActorPrincipal {
        principal: format!("user:{}", usr(n)),
        actor: ActorRef::Human { user_id: usr(n) },
        credential_ref: format!("pat-{n}"),
    }
}
fn claim(
    store: &MemoryExecutionStore,
    directory: &MemoryDirectory,
    who: &ActorPrincipal,
    step: &runbook_model::StepRunId,
    n: u8,
) -> Result<runbook_model::ClaimRef, runbook_core::actor::ActorError> {
    ready(claim_step(
        ready(begin_actor(store, directory, scope(), &exe())).unwrap(),
        who,
        step,
        &ClaimRequest {
            take_over: false,
            session_id: format!("s{n}"),
            actor: None,
        },
        format!("clm_{n:026}").parse().unwrap(),
        None,
    ))
}

/// C4 (PRD §6 handoff): an agent's failed task is handed to the owners. The
/// human continuation is offered to people, not agents, and only an
/// eligible human may claim it.
#[test]
fn handed_off_agent_task_is_human_work() {
    let (store, directory) = setup(HANDOFF);
    let probe = step_id(&state(&store, &directory), "probe");
    let agent_claim = claim(&store, &directory, &agent(), &probe, 1).unwrap();
    ready(report_step(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &agent(),
        &ReportRequest {
            step_run_id: probe.clone(),
            claim: agent_claim,
            status: ReportStatus::Failed,
            verdict: None,
            outputs: Data::new(),
            summary: Some("cannot reach the host".into()),
            evidence: vec![],
            logs: vec![],
            cost: None,
        },
        None,
    ))
    .unwrap();
    let people = resume_brief(&state(&store, &directory), Some(ActorKind::Human));
    assert_eq!(
        people
            .ready_work
            .iter()
            .map(|w| (w.path.as_str(), w.actor_kind))
            .collect::<Vec<_>>(),
        vec![("probe", ActorKind::Human)]
    );
    let agents = resume_brief(&state(&store, &directory), Some(ActorKind::Agent));
    assert!(agents.ready_work.is_empty(), "{:?}", agents.ready_work);
    // The owners' inbox lists the handed-off step.
    let inbox: Vec<_> = ready(store.list_assignments())
        .unwrap()
        .into_iter()
        .filter(|a| a.step_run_id == probe)
        .map(|a| (a.kind, a.recipients))
        .collect();
    assert_eq!(
        inbox,
        vec![(
            runbook_core::catalog::AssignmentKind::Handoff,
            vec![runbook_model::RecipientRef::Team(team(1).to_string())]
        )]
    );
    // Agents cannot take it back; a human outside the owners cannot claim it.
    assert!(matches!(
        claim(&store, &directory, &agent(), &probe, 2),
        Err(runbook_core::actor::ActorError::Rejected {
            code: "claim.actor_mismatch",
            ..
        })
    ));
    assert!(matches!(
        claim(&store, &directory, &human(3), &probe, 3),
        Err(runbook_core::actor::ActorError::Rejected {
            code: "auth.forbidden",
            ..
        })
    ));
    claim(&store, &directory, &human(1), &probe, 4).unwrap();
}

/// LANG7 review round 1: a strict `actor: any` task is its assignee's, a
/// person's, so the brief offers it to people only (and never as agent work
/// to an audience-less reader); a strict agent task stays agent work.
#[test]
fn strict_any_task_is_human_work() {
    let (store, directory) = setup(
        "metadata: {id: gate-check, title: Strict any}\n\
         spec:\n  steps:\n\
         \x20   - id: all\n      parallel:\n\
         \x20       - {id: page, actor: any, assignee: team:oncall, assignment: strict}\n\
         \x20       - {id: build, actor: agent, assignee: team:oncall, assignment: strict}\n",
    );
    let paths = |audience| {
        resume_brief(&state(&store, &directory), audience)
            .ready_work
            .into_iter()
            .map(|w| (w.path, w.actor_kind))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        paths(Some(ActorKind::Agent)),
        vec![("build".to_string(), ActorKind::Agent)]
    );
    assert_eq!(
        paths(Some(ActorKind::Human)),
        vec![("page".to_string(), ActorKind::Human)]
    );
    assert_eq!(
        paths(None),
        vec![
            ("page".to_string(), ActorKind::Human),
            ("build".to_string(), ActorKind::Agent)
        ]
    );
}
