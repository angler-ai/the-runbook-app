//! R18: inbox assignments derived from settled state. Escalation adds
//! recipients rather than replacing them, a voter leaves the inbox while
//! recipients stay the full eligible set, and an assignment keeps its
//! timestamps across unrelated commits (D33 newest-assignment order).
use runbook_core::{
    actor::{ActorPrincipal, begin_actor},
    catalog::{
        Assignment, AssignmentKind, CatalogStore, RunbookOrigin, RunbookRecord, TenantScope,
    },
    control::{set_paused, tick},
    decisions::vote,
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, TeamRecord, UserRecord,
        memory::MemoryDirectory,
    },
    inbox::{InboxFilters, inbox},
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, activate, prepare_start_commit},
    work_queue::WorkQueue,
};
use runbook_model::{
    ActorRef, Data, DecisionRequest, ExecutionId, ProjectRole, RecipientRef, RunbookId, TeamId,
    UserId,
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
fn team(n: u8) -> TeamId {
    format!("team_{n:026}").parse().unwrap()
}
fn exe() -> ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}
fn human(n: u8) -> ActorPrincipal {
    ActorPrincipal {
        principal: format!("user:{}", usr(n)),
        actor: ActorRef::Human { user_id: usr(n) },
        credential_ref: "pat".into(),
    }
}

/// oncall = users 1 and 2; leads = user 3. Two votes are needed, so one
/// vote leaves the approval open.
const ESCALATING: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gate-check, title: Gate check}\n\
    spec:\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Go?\", options: [keep], approvers: {teams: [oncall]}, min: 2, timeout: 1m, on_timeout: escalate, escalate_to: [\"team:leads\"], escalation_timeout: 1h}\n    \
    - id: after\n      title: After\n";
const PLAIN: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gate-check, title: Gate check}\n\
    spec:\n  execution: {timeout: 1h}\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Go?\", options: [keep], approvers: {teams: [oncall]}, min: 2}\n    \
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

fn approval(store: &MemoryExecutionStore) -> Assignment {
    let mut found: Vec<Assignment> = ready(store.list_assignments())
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == AssignmentKind::Approval)
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}
fn inbox_of(store: &MemoryExecutionStore, directory: &MemoryDirectory, n: u8) -> usize {
    let snapshot = ready(directory.snapshot(&scope().org_id)).unwrap();
    ready(inbox(
        store,
        &scope(),
        &snapshot,
        &usr(n),
        InboxFilters::default(),
        None,
        10,
    ))
    .unwrap()
    .assignments
    .len()
}
fn cast(store: &MemoryExecutionStore, directory: &MemoryDirectory, n: u8) {
    let w = ready(begin_actor(store, directory, scope(), &exe())).unwrap();
    let gate = w
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "gate")
        .and_then(|s| s.id.clone())
        .unwrap();
    ready(vote(
        w,
        &human(n),
        &gate,
        &DecisionRequest {
            option: "keep".into(),
            comment: None,
            session_id: None,
            actor: None,
        },
        None,
    ))
    .unwrap();
}

#[test]
fn escalation_adds_recipients_to_original_approvers() {
    let (store, directory) = setup(ESCALATING);
    let user = |n: u8| RecipientRef::User(usr(n));
    assert_eq!(approval(&store).recipients, vec![user(1), user(2)]);
    store.set_time(1_000 + 61_000);
    ready(tick(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
    ))
    .unwrap();
    let escalated = approval(&store);
    assert!(
        escalated.recipients.contains(&user(1)) && escalated.recipients.contains(&user(2)),
        "original approvers stay eligible during escalation: {escalated:?}"
    );
    assert!(
        escalated
            .recipients
            .contains(&RecipientRef::Team(team(2).to_string())),
        "{escalated:?}"
    );
    assert_eq!(inbox_of(&store, &directory, 1), 1);
    assert_eq!(inbox_of(&store, &directory, 3), 1);
    // User 3 is reached only through the escalation team; after voting the
    // approval (still short of quorum) leaves their inbox (R18b).
    cast(&store, &directory, 3);
    assert_eq!(approval(&store).voted_by, vec![usr(3)]);
    assert_eq!(inbox_of(&store, &directory, 3), 0);
    assert_eq!(inbox_of(&store, &directory, 1), 1);
}

#[test]
fn voter_reached_through_team_leaves_inbox() {
    let (store, directory) = setup(PLAIN);
    assert_eq!(inbox_of(&store, &directory, 1), 1);
    assert_eq!(inbox_of(&store, &directory, 2), 1);
    cast(&store, &directory, 1);
    assert_eq!(approval(&store).voted_by, vec![usr(1)]);
    assert_eq!(inbox_of(&store, &directory, 1), 0, "user 1 already voted");
    assert_eq!(inbox_of(&store, &directory, 2), 1, "teammate still owes");
}

#[test]
fn unrelated_commits_keep_assignment_timestamps() {
    let (store, directory) = setup(PLAIN);
    let first = approval(&store);
    store.set_time(50_000);
    ready(set_paused(
        ready(begin_actor(&store, &directory, scope(), &exe())).unwrap(),
        &human(1),
        true,
        None,
    ))
    .unwrap();
    store.set_time(90_000);
    cast(&store, &directory, 2);
    let later = approval(&store);
    assert_eq!(later.recipients, first.recipients);
    assert_eq!(
        (later.updated_at_ms, later.sequence),
        (first.updated_at_ms, first.sequence),
        "a pause and a partial vote do not make the assignment newer (D33)"
    );
}
