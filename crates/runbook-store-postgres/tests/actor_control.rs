//! B6a storage/engine parts: claim fencing, votes, pause/cancel, signals.
mod support;
use runbook_core::{
    ExecutionStore,
    actor::{ActorPrincipal, begin_actor},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord},
    claims::ClaimStore,
    claims::{append_logs, claim_step, heartbeat, release_claim, report_step},
    control::{cancel, set_paused, signal},
    decisions::{agent_decide, vote},
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, TeamRecord, UserRecord,
    },
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, prepare_start_commit},
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{
    ActorRef, ChecklistRequest, ChecklistTick, ClaimId, ClaimRequest, Data, DecisionRequest,
    ExecutionId, HeartbeatRequest, LogsRequest, ProjectRole, ReportRequest, ReportStatus,
    SignalRequest, StepStatus,
};
use runbook_store_conformance::scope;
use runbook_store_postgres::{PostgresDirectory, PostgresExecutionStore};
use support::factory::{PostgresFactory, with_factory};

fn rb(n: u8) -> runbook_model::RunbookId {
    format!("rb_{n:026}").parse().unwrap()
}
fn exe(n: u8) -> ExecutionId {
    format!("exe_{n:026}").parse().unwrap()
}
fn usr(n: u8) -> runbook_model::UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn human(n: u8) -> ActorPrincipal {
    ActorPrincipal {
        principal: format!("user:{}", usr(n)),
        actor: ActorRef::Human { user_id: usr(n) },
        credential_ref: format!("pat-{n}"),
    }
}
fn agent(name: &str) -> ActorPrincipal {
    ActorPrincipal {
        principal: "service:runner".into(),
        actor: ActorRef::Agent {
            principal_id: "runner".into(),
            session_id: "s1".into(),
            name: name.into(),
            model: None,
            service_attribution: None,
        },
        credential_ref: "svc-1".into(),
    }
}

const TWO_TASKS: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: actor-check, title: Actor check}\n\
    spec:\n  steps:\n    \
    - id: first\n      title: First\n      checklist: [One, Two]\n      outputs: {count: {type: integer}}\n    \
    - id: second\n      title: Second\n";

async fn directory(f: &PostgresFactory) -> PostgresDirectory {
    let directory = f.fresh_directory();
    let org = scope(1).org_id;
    let mut mutations = vec![
        DirectoryMutation::CreateOrganization {
            title: "Org".into(),
        },
        DirectoryMutation::CreateProject(ProjectRecord {
            id: scope(1).project_id,
            org_id: org.clone(),
            slug: "api".into(),
            title: "API".into(),
        }),
        DirectoryMutation::CreateTeam(TeamRecord {
            id: "team_00000000000000000000000001".parse().unwrap(),
            org_id: org.clone(),
            slug: "oncall".into(),
            title: "Oncall".into(),
        }),
    ];
    for (n, name) in [(1, "alice"), (2, "bob"), (3, "admin")] {
        mutations.push(DirectoryMutation::CreateUser(UserRecord {
            id: usr(n),
            name: name.into(),
            active: true,
            role: runbook_core::directory::OrgRole::Member,
        }));
        mutations.push(DirectoryMutation::GrantProjectRole {
            project_id: scope(1).project_id,
            user_id: usr(n),
            role: if n == 3 {
                ProjectRole::Admin
            } else {
                ProjectRole::Operator
            },
        });
        if n < 3 {
            mutations.push(DirectoryMutation::AddTeamMember {
                team_id: "team_00000000000000000000000001".parse().unwrap(),
                user_id: usr(n),
            });
        }
    }
    directory
        .commit(DirectoryCommit {
            org_id: org,
            expected_revision: 0,
            mutations,
        })
        .await
        .unwrap();
    directory
}

async fn started(
    f: &PostgresFactory,
    source: &str,
    slug: &str,
) -> (PostgresExecutionStore, PostgresDirectory) {
    let store = f.fresh_store(scope(1)).await;
    let directory = directory(f).await;
    store
        .create_runbook(
            RunbookRecord {
                runbook_id: rb(1),
                slug: slug.into(),
                origin: RunbookOrigin::Ui,
                archived: false,
            },
            source.into(),
        )
        .await
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
        slug,
        source,
        &context,
        "rbv_00000000000000000000000001".parse().unwrap(),
    )
    .unwrap();
    publish_draft(&store, &store, &rb(1), 1, prepared)
        .await
        .unwrap();
    let command = StartCommand {
        execution_id: exe(1),
        runbook_id: rb(1),
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: "start-1".into(),
        principal: "user:alice".into(),
        request_hash: [1; 32],
    };
    let commit = prepare_start_commit(&store, &store, &command)
        .await
        .unwrap();
    store.commit_start(commit).await.unwrap();
    let claims = store.claim_due(10, 60_000).await.unwrap();
    let activation = claims
        .iter()
        .find(|lease| lease.id.kind == WorkKind::Activation)
        .expect("activation work");
    runbook_core::start::activate(&store, &directory, scope(1), activation)
        .await
        .unwrap();
    (store, directory)
}

async fn work(
    store: &PostgresExecutionStore,
    directory: &PostgresDirectory,
) -> runbook_core::actor::ActorWork<PostgresExecutionStore> {
    begin_actor(store, directory, scope(1), &exe(1))
        .await
        .unwrap()
}
fn step_run(
    work: &runbook_core::actor::ActorWork<PostgresExecutionStore>,
    step_id: &str,
) -> runbook_model::StepRunId {
    work.runtime
        .state
        .steps
        .iter()
        .find(|step| step.step_id == step_id && step.id.is_some())
        .and_then(|step| step.id.clone())
        .expect("materialized step")
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn claim_generation_fences_every_actor_write() {
    with_factory(async |f| {
        let (store, directory) = started(f, TWO_TASKS, "actor-check").await;
        let w = work(&store, &directory).await;
        let first = step_run(&w, "first");
        let claim_id: ClaimId = "clm_00000000000000000000000001".parse().unwrap();
        let claimed = claim_step(
            w,
            &agent("bot"),
            &first,
            &ClaimRequest {
                take_over: false,
                session_id: "s1".into(),
                actor: None,
            },
            claim_id.clone(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(claimed.generation.get(), 1);
        // A second contender cannot claim the claimed step.
        let w = work(&store, &directory).await;
        assert!(
            claim_step(
                w,
                &agent("other"),
                &first,
                &ClaimRequest {
                    take_over: false,
                    session_id: "s2".into(),
                    actor: None
                },
                "clm_00000000000000000000000002".parse().unwrap(),
                None,
            )
            .await
            .is_err()
        );
        // Heartbeat and logs extend the live lease.
        let before = store
            .claim_record(&exe(1), &first)
            .await?
            .unwrap()
            .lease_deadline_at_ms
            .unwrap();
        let after = heartbeat(
            &store,
            &agent("bot"),
            &exe(1),
            &first,
            &HeartbeatRequest {
                claim: claimed.clone(),
            },
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(after >= before);
        append_logs(
            &store,
            &agent("bot"),
            &exe(1),
            &first,
            &LogsRequest {
                claim: claimed.clone(),
                logs: vec!["working".into()],
            },
            None,
        )
        .await
        .unwrap();
        // Checklist requires every item before a successful report (422).
        let w = work(&store, &directory).await;
        let refused = report_step(
            w,
            &agent("bot"),
            &ReportRequest {
                step_run_id: first.clone(),
                claim: claimed.clone(),
                status: ReportStatus::Succeeded,
                verdict: None,
                outputs: Data::from([("count".to_string(), serde_json::json!("many"))]),
                summary: None,
                evidence: vec![],
                logs: vec![],
                cost: None,
            },
            None,
        )
        .await;
        assert!(refused.is_err(), "wrong-type outputs must reject");
        assert_eq!(store.load(&exe(1)).await?.revision, 4);
        let w = work(&store, &directory).await;
        runbook_core::claims::update_checklist(
            w,
            &agent("bot"),
            &first,
            &ChecklistRequest {
                claim: claimed.clone(),
                checklist: vec![
                    ChecklistTick {
                        title: "One".into(),
                        completed: true,
                    },
                    ChecklistTick {
                        title: "Two".into(),
                        completed: true,
                    },
                ],
            },
            None,
        )
        .await
        .unwrap();
        // Expire the agent lease: work returns to ready, old writes fence.
        f.admin
            .client()
            .execute(
                "UPDATE runbook.execution_claim SET lease_deadline_at_ms = 0, \
                 body = jsonb_set(body::jsonb, '{lease_deadline_at_ms}', '0'::jsonb)::text \
                 WHERE execution_id = $1",
                &[&exe(1).as_str()],
            )
            .await
            .unwrap();
        let leases = store.claim_due(10, 60_000).await?;
        let expiry = leases
            .iter()
            .find(|lease| matches!(lease.id.kind, WorkKind::ClaimLease { .. }))
            .expect("claim lease work")
            .clone();
        let w = work(&store, &directory).await;
        assert!(
            runbook_core::claims::expire_claim(
                w,
                &first,
                match &expiry.id.kind {
                    WorkKind::ClaimLease { generation, .. } => *generation,
                    _ => unreachable!(),
                },
            )
            .await
            .unwrap()
        );
        store.ack(&expiry.id, expiry.generation).await?;
        let w = work(&store, &directory).await;
        assert_eq!(
            w.runtime
                .state
                .steps
                .iter()
                .find(|s| s.step_id == "first")
                .unwrap()
                .status,
            StepStatus::Ready
        );
        drop(w);
        assert!(
            store
                .load(&exe(1))
                .await?
                .outbox
                .values()
                .any(|intent| intent.reason.as_deref() == Some("lease_expired")),
            "expiry emits a lease_expired work intent"
        );
        // The replacement generation rejects every old-generation write.
        let w = work(&store, &directory).await;
        let second = claim_step(
            w,
            &agent("other"),
            &first,
            &ClaimRequest {
                take_over: false,
                session_id: "s2".into(),
                actor: None,
            },
            "clm_00000000000000000000000002".parse().unwrap(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(second.generation.get(), 2);
        assert!(
            heartbeat(
                &store,
                &agent("bot"),
                &exe(1),
                &first,
                &HeartbeatRequest {
                    claim: claimed.clone()
                },
                None
            )
            .await
            .is_err()
        );
        assert!(
            append_logs(
                &store,
                &agent("bot"),
                &exe(1),
                &first,
                &LogsRequest {
                    claim: claimed.clone(),
                    logs: vec!["late".into()]
                },
                None
            )
            .await
            .is_err()
        );
        let w = work(&store, &directory).await;
        assert!(
            report_step(
                w,
                &agent("bot"),
                &ReportRequest {
                    step_run_id: first.clone(),
                    claim: claimed.clone(),
                    status: ReportStatus::Succeeded,
                    verdict: None,
                    outputs: Data::from([("count".to_string(), serde_json::json!(1))]),
                    summary: None,
                    evidence: vec![],
                    logs: vec![],
                    cost: None,
                },
                None,
            )
            .await
            .is_err()
        );
        // Human claims do not expire; a released claim cannot report.
        let w = work(&store, &directory).await;
        release_claim(w, &agent("other"), &first, &second, None)
            .await
            .unwrap();
        let w = work(&store, &directory).await;
        let human_claim = claim_step(
            w,
            &human(1),
            &first,
            &ClaimRequest {
                take_over: false,
                session_id: "web".into(),
                actor: None,
            },
            "clm_00000000000000000000000003".parse().unwrap(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .claim_record(&exe(1), &first)
                .await?
                .unwrap()
                .lease_deadline_at_ms,
            None
        );
        let w = work(&store, &directory).await;
        release_claim(w, &human(1), &first, &human_claim, None)
            .await
            .unwrap();
        let w = work(&store, &directory).await;
        assert!(
            report_step(
                w,
                &human(1),
                &ReportRequest {
                    step_run_id: first.clone(),
                    claim: human_claim.clone(),
                    status: ReportStatus::Succeeded,
                    verdict: None,
                    outputs: Data::from([("count".to_string(), serde_json::json!(1))]),
                    summary: None,
                    evidence: vec![],
                    logs: vec![],
                    cost: None,
                },
                None,
            )
            .await
            .is_err()
        );
        // Same owner, stale generation: only the generation fence rejects.
        let w = work(&store, &directory).await;
        let reclaimed = claim_step(
            w,
            &human(1),
            &first,
            &ClaimRequest {
                take_over: false,
                session_id: "web".into(),
                actor: None,
            },
            "clm_00000000000000000000000003".parse().unwrap(),
            None,
        )
        .await
        .unwrap();
        assert!(reclaimed.generation.get() > human_claim.generation.get());
        let w = work(&store, &directory).await;
        let stale = report_step(
            w,
            &human(1),
            &ReportRequest {
                step_run_id: first.clone(),
                claim: human_claim,
                status: ReportStatus::Succeeded,
                verdict: None,
                outputs: Data::from([("count".to_string(), serde_json::json!(1))]),
                summary: None,
                evidence: vec![],
                logs: vec![],
                cost: None,
            },
            None,
        )
        .await;
        assert!(
            matches!(&stale, Err(error) if error.code() == "claim.stale_generation"),
            "{stale:?}"
        );
        Ok(())
    })
    .await;
}

const GATE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gate-check, title: Gate check}\n\
    spec:\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Go?\", options: [keep, stop], approvers: {teams: [oncall]}, min: 2}\n    \
    - id: after\n      title: After\n";

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn votes_use_current_membership_and_deadline() {
    with_factory(async |f| {
        let (store, directory) = started(f, GATE, "gate-check").await;
        let w = work(&store, &directory).await;
        let gate = step_run(&w, "gate");
        drop(w);
        // The admin is not a target; alice (oncall) is.
        assert!(
            vote(
                work(&store, &directory).await,
                &human(3),
                &gate,
                &DecisionRequest {
                    option: "keep".into(),
                    comment: None,
                    session_id: None,
                    actor: None,
                },
                None,
            )
            .await
            .is_err()
        );
        vote(
            work(&store, &directory).await,
            &human(1),
            &gate,
            &DecisionRequest {
                option: "keep".into(),
                comment: None,
                session_id: None,
                actor: None,
            },
            None,
        )
        .await
        .unwrap();
        // A second token for the same human is not a second signature.
        let mut alice2 = human(1);
        alice2.credential_ref = "pat-second".into();
        let w = work(&store, &directory).await;
        let _ = vote(
            w,
            &alice2,
            &gate,
            &DecisionRequest {
                option: "keep".into(),
                comment: None,
                session_id: None,
                actor: None,
            },
            None,
        )
        .await;
        let w = work(&store, &directory).await;
        let approval = w
            .runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == "gate")
            .and_then(|s| s.approval.as_ref())
            .expect("approval state");
        assert_eq!(approval.distinct_votes, 1);
        assert!(approval.decision.is_none());
        drop(w);
        // Removing bob from oncall rejects his vote; alice's signature stays.
        let revision = directory.snapshot(&scope(1).org_id).await.unwrap().revision;
        directory
            .commit(DirectoryCommit {
                org_id: scope(1).org_id,
                expected_revision: revision,
                mutations: vec![DirectoryMutation::RemoveTeamMember {
                    team_id: "team_00000000000000000000000001".parse().unwrap(),
                    user_id: usr(2),
                }],
            })
            .await
            .unwrap();
        let w = work(&store, &directory).await;
        assert!(
            vote(
                w,
                &human(2),
                &gate,
                &DecisionRequest {
                    option: "keep".into(),
                    comment: None,
                    session_id: None,
                    actor: None,
                },
                None,
            )
            .await
            .is_err()
        );
        let w = work(&store, &directory).await;
        let approval = w
            .runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == "gate")
            .and_then(|s| s.approval.as_ref())
            .unwrap();
        assert_eq!(approval.votes.len(), 1);
        assert_eq!(approval.votes[0].vote.eligibility.directory_revision, 1);
        drop(w);
        Ok(())
    })
    .await;
}

const PARALLEL: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: parallel-check, title: Parallel check}\n\
    spec:\n  steps:\n    - id: both\n      parallel: [{id: left, title: Left}, {id: right, title: Right}]\n";

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn pause_and_cancel_preserve_lifecycle() {
    with_factory(async |f| {
        let (store, directory) = started(f, PARALLEL, "parallel-check").await;
        let w = work(&store, &directory).await;
        let left = step_run(&w, "left");
        let claim = claim_step(
            w,
            &human(1),
            &left,
            &ClaimRequest {
                take_over: false,
                session_id: "web".into(),
                actor: None,
            },
            "clm_00000000000000000000000001".parse().unwrap(),
            None,
        )
        .await
        .unwrap();
        set_paused(work(&store, &directory).await, &human(3), true, None)
            .await
            .unwrap();
        // Ordinary claims fail while paused; existing claims still work.
        let w = work(&store, &directory).await;
        let right = step_run(&w, "right");
        assert!(
            claim_step(
                w,
                &human(2),
                &right,
                &ClaimRequest {
                    take_over: false,
                    session_id: "web".into(),
                    actor: None
                },
                "clm_00000000000000000000000002".parse().unwrap(),
                None,
            )
            .await
            .is_err()
        );
        heartbeat(
            &store,
            &human(1),
            &exe(1),
            &left,
            &HeartbeatRequest {
                claim: claim.clone(),
            },
            None,
        )
        .await
        .unwrap();
        let w = work(&store, &directory).await;
        report_step(
            w,
            &human(1),
            &ReportRequest {
                step_run_id: left.clone(),
                claim: claim.clone(),
                status: ReportStatus::Succeeded,
                verdict: None,
                outputs: Data::new(),
                summary: None,
                evidence: vec![],
                logs: vec![],
                cost: None,
            },
            None,
        )
        .await
        .unwrap();
        set_paused(work(&store, &directory).await, &human(3), false, None)
            .await
            .unwrap();
        let w = work(&store, &directory).await;
        assert!(
            claim_step(
                w,
                &human(2),
                &right,
                &ClaimRequest {
                    take_over: false,
                    session_id: "web".into(),
                    actor: None
                },
                "clm_00000000000000000000000002".parse().unwrap(),
                None,
            )
            .await
            .is_ok()
        );
        // Running cancellation enters and completes cleanup.
        cancel(&store, work(&store, &directory).await, &human(3), None)
            .await
            .unwrap();
        assert_eq!(
            store.load(&exe(1)).await?.projection.status,
            runbook_model::ExecutionStatus::Cancelled
        );
        Ok(())
    })
    .await;
}

const WAITER: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: wait-check, title: Wait check}\n\
    spec:\n  steps:\n    - id: first\n      title: First\n    \
    - id: hold\n      wait: {signal: go, timeout: 1h}\n    - id: after\n      title: After\n";

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn signal_is_current_and_idempotent() {
    with_factory(async |f| {
        let (store, directory) = started(f, WAITER, "wait-check").await;
        // Early and wrong-name signals find no current waiter.
        for name in ["go", "wrong"] {
            assert!(
                signal(
                    work(&store, &directory).await,
                    &human(1),
                    name,
                    "sig-early",
                    &SignalRequest {
                        payload: Data::new()
                    },
                    None,
                )
                .await
                .is_err()
            );
        }
        let w = work(&store, &directory).await;
        let first = step_run(&w, "first");
        let claim = claim_step(
            w,
            &human(1),
            &first,
            &ClaimRequest {
                take_over: false,
                session_id: "web".into(),
                actor: None,
            },
            "clm_00000000000000000000000001".parse().unwrap(),
            None,
        )
        .await
        .unwrap();
        let w = work(&store, &directory).await;
        report_step(
            w,
            &human(1),
            &ReportRequest {
                step_run_id: first,
                claim,
                status: ReportStatus::Succeeded,
                verdict: None,
                outputs: Data::new(),
                summary: None,
                evidence: vec![],
                logs: vec![],
                cost: None,
            },
            None,
        )
        .await
        .unwrap();
        // Wrong tenant cannot even open the execution.
        let foreign = f.fresh_store(scope(2)).await;
        assert!(
            begin_actor(&foreign, &directory, scope(2), &exe(1))
                .await
                .is_err()
        );
        signal(
            work(&store, &directory).await,
            &human(1),
            "go",
            "sig-1",
            &SignalRequest {
                payload: Data::new(),
            },
            None,
        )
        .await
        .unwrap();
        let w = work(&store, &directory).await;
        let hold = w
            .runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == "hold")
            .unwrap();
        assert_eq!(hold.status, StepStatus::Succeeded);
        let after = w
            .runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == "after")
            .unwrap();
        assert_eq!(after.status, StepStatus::Ready);
        drop(w);
        // The same signal ID replays without a second wake or event.
        let before = store.load(&exe(1)).await?.events.len();
        signal(
            work(&store, &directory).await,
            &human(1),
            "go",
            "sig-1",
            &SignalRequest {
                payload: Data::new(),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(store.load(&exe(1)).await?.events.len(), before);
        Ok(())
    })
    .await;
}

const AGENT_GATE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: agent-gate, title: Agent gate}\n\
    spec:\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Go?\", options: [keep], approvers: {teams: [oncall]}, min: 1, allow_agents: true}\n    \
    - id: after\n      title: After\n";

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn agent_decision_uses_agent_provenance() {
    with_factory(async |f| {
        let (store, directory) = started(f, AGENT_GATE, "agent-gate").await;
        let w = work(&store, &directory).await;
        let gate = step_run(&w, "gate");
        let recorded = agent_decide(
            &store,
            w,
            &agent("bot"),
            &gate,
            &DecisionRequest {
                option: "keep".into(),
                comment: None,
                session_id: None,
                actor: None,
            },
            "s1",
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_value(recorded).unwrap(),
            serde_json::json!({"by":"agent","agent_name":"bot","model":null,"session_id":"s1","option":"keep","comment":null})
        );
        let w = work(&store, &directory).await;
        let approval = w
            .runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == "gate")
            .and_then(|s| s.approval.as_ref())
            .unwrap();
        assert!(approval.agent_decision.is_some());
        assert!(approval.votes.is_empty());
        assert_eq!(
            w.runtime
                .state
                .steps
                .iter()
                .find(|s| s.step_id == "gate")
                .unwrap()
                .status,
            StepStatus::Succeeded
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn actor_replay_is_atomic_for_events_lease_and_logs() {
    with_factory(async |f| {
        use runbook_core::{WriteStage, actor::Replay};
        let source = TWO_TASKS.replace("      checklist: [One, Two]\n", "");
        let (store, directory) = started(f, &source, "actor-check").await;
        let actor = agent("bot");
        let no_op = Replay {
            key: "noop".into(),
            principal: actor.principal.clone(),
            request_hash: [2; 32],
        };
        let initial = store.load(&exe(1)).await.unwrap().revision;
        set_paused(work(&store, &directory).await, &actor, false, Some(&no_op))
            .await
            .unwrap();
        assert_eq!(store.load(&exe(1)).await.unwrap().revision, initial);

        let w = work(&store, &directory).await;
        let step = step_run(&w, "first");
        let replay = |key: &str| Replay {
            key: key.into(),
            principal: actor.principal.clone(),
            request_hash: [4; 32],
        };
        let claim_request = ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        };
        let claim = claim_step(
            w,
            &actor,
            &step,
            &claim_request,
            "clm_00000000000000000000000001".parse().unwrap(),
            Some(&replay("claim")),
        )
        .await
        .unwrap();
        let revision = store.load(&exe(1)).await.unwrap().revision;
        let again = claim_step(
            work(&store, &directory).await,
            &actor,
            &step,
            &claim_request,
            "clm_00000000000000000000000002".parse().unwrap(),
            Some(&replay("claim")),
        )
        .await
        .unwrap();
        assert_eq!(again, claim);
        assert_eq!(store.load(&exe(1)).await.unwrap().revision, revision);
        let heartbeat_request = HeartbeatRequest {
            claim: claim.clone(),
        };
        let before_heartbeat = store.claim_record(&exe(1), &step).await.unwrap().unwrap();
        store.fail_after(WriteStage::Idempotency);
        assert!(
            heartbeat(
                &store,
                &actor,
                &exe(1),
                &step,
                &heartbeat_request,
                Some(&replay("heartbeat"))
            )
            .await
            .is_err()
        );
        assert_eq!(
            store.claim_record(&exe(1), &step).await.unwrap().unwrap(),
            before_heartbeat
        );
        let deadline = heartbeat(
            &store,
            &actor,
            &exe(1),
            &step,
            &heartbeat_request,
            Some(&replay("heartbeat")),
        )
        .await
        .unwrap();
        assert_eq!(
            heartbeat(
                &store,
                &actor,
                &exe(1),
                &step,
                &heartbeat_request,
                Some(&replay("heartbeat"))
            )
            .await
            .unwrap(),
            deadline
        );
        let logs = LogsRequest {
            claim: claim.clone(),
            logs: vec!["once".into()],
        };
        store.fail_after(WriteStage::Idempotency);
        assert!(
            append_logs(&store, &actor, &exe(1), &step, &logs, Some(&replay("logs")))
                .await
                .is_err()
        );
        assert_eq!(
            append_logs(&store, &actor, &exe(1), &step, &logs, Some(&replay("logs")))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            append_logs(&store, &actor, &exe(1), &step, &logs, Some(&replay("logs")))
                .await
                .unwrap(),
            1
        );
        let request = ReportRequest {
            step_run_id: step.clone(),
            claim: claim.clone(),
            status: ReportStatus::Succeeded,
            verdict: None,
            outputs: [("count".into(), serde_json::json!(1))].into(),
            summary: None,
            evidence: vec![],
            logs: vec![],
            cost: None,
        };
        store.fail_after(WriteStage::Idempotency);
        assert!(
            report_step(
                work(&store, &directory).await,
                &actor,
                &request,
                Some(&replay("report"))
            )
            .await
            .is_err()
        );
        assert_eq!(store.load(&exe(1)).await.unwrap().revision, revision);
        report_step(
            work(&store, &directory).await,
            &actor,
            &request,
            Some(&replay("report")),
        )
        .await
        .unwrap();
        let completed = store.load(&exe(1)).await.unwrap().revision;
        report_step(
            work(&store, &directory).await,
            &actor,
            &request,
            Some(&replay("report")),
        )
        .await
        .unwrap();
        assert_eq!(store.load(&exe(1)).await.unwrap().revision, completed);
        // Saved lease/log responses are returned before the now-released claim fence.
        assert_eq!(
            append_logs(&store, &actor, &exe(1), &step, &logs, Some(&replay("logs")))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            heartbeat(
                &store,
                &actor,
                &exe(1),
                &step,
                &heartbeat_request,
                Some(&replay("heartbeat"))
            )
            .await
            .unwrap(),
            deadline
        );
        let mut changed = replay("logs");
        changed.request_hash = [5; 32];
        assert_eq!(
            append_logs(&store, &actor, &exe(1), &step, &logs, Some(&changed))
                .await
                .unwrap_err()
                .code(),
            "request.idempotency_key_reused"
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires owned PostgreSQL database"]
async fn step_run_lookup_is_org_scoped() {
    use runbook_core::step_lookup::StepRunLookup;
    with_factory(async |f| {
        let (store, directory) = started(f, TWO_TASKS, "actor-check").await;
        let work = work(&store, &directory).await;
        let sr = step_run(&work, "first");
        let exe = work.runtime.state.id.clone();
        drop(work);
        assert_eq!(
            store
                .step_run_execution(&scope(1).org_id, &sr)
                .await
                .unwrap(),
            Some((exe, scope(1).project_id))
        );
        let other = "org_00000000000000000000000002".parse().unwrap();
        assert_eq!(store.step_run_execution(&other, &sr).await.unwrap(), None);
        let missing = "sr_00000000000000000000000099".parse().unwrap();
        assert_eq!(
            store
                .step_run_execution(&scope(1).org_id, &missing)
                .await
                .unwrap(),
            None
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires owned PostgreSQL database"]
async fn metadata_and_queued_cancel_are_atomic() {
    use runbook_core::control::patch;
    use runbook_model::{ExecutionAction, ExecutionPatchRequest, ExecutionStatus};
    with_factory(async |f| {
        let source = TWO_TASKS.replace("  steps:", "  execution: {concurrency: {key: api, limit: 1}}\n  fields:\n    severity: {type: integer, default: 1, indexed: true}\n    fixed: {type: string, default: pinned, locked: true}\n  steps:");
        let (store, directory) = started(f, &source, "actor-check").await;
        let request = ExecutionPatchRequest { action: Some(ExecutionAction::Pause), title: Some("Incident".into()), tags: Some(vec!["urgent".into()]), fields: Some([("severity".into(), 3.into())].into()) };
        let before = store.load(&exe(1)).await?;
        store.fail_after(runbook_core::WriteStage::DerivedRows);
        assert!(patch(work(&store,&directory).await, &human(1), &request, None).await.is_err());
        assert_eq!(store.load(&exe(1)).await?.revision,before.revision);
        patch(work(&store,&directory).await, &human(1), &request, None).await.unwrap();
        let summary=store.get_run(&exe(1)).await?.unwrap();
        assert_eq!(summary.title,"Incident"); assert_eq!(summary.fields["severity"],3); assert_eq!(summary.tags,["urgent"]);
        assert_eq!(summary.status, ExecutionStatus::Paused);
        let revision=store.load(&exe(1)).await?.revision;
        patch(work(&store,&directory).await,&human(1),&request,None).await.unwrap();
        assert_eq!(store.load(&exe(1)).await?.revision,revision);
        let invalid = ExecutionPatchRequest { fields: Some([("fixed".into(), "changed".into())].into()), ..request };
        assert!(patch(work(&store,&directory).await,&human(1),&invalid,None).await.is_err());
        assert_eq!(store.load(&exe(1)).await?.revision,revision);
        let command = StartCommand { execution_id: exe(2), runbook_id: rb(1), version: None, inputs: Data::new(), fields: Data::new(), project_vars: Data::new(), title: Some("Queued".into()), tags: vec![], url: String::new(), created_by: "alice".into(), limits: Default::default(), idempotency_key: "q2".into(), principal: "user:alice".into(), request_hash: [2;32] };
        store.commit_start(prepare_start_commit(&store,&store,&command).await.unwrap()).await?;
        let request = ExecutionPatchRequest { action: Some(ExecutionAction::Cancel), title: None, tags: None, fields: None };
        let w=begin_actor(&store,&directory,scope(1),&exe(2)).await.unwrap();
        store.fail_after(runbook_core::WriteStage::DerivedRows);
        assert!(patch(w,&human(1),&request,None).await.is_err());
        assert_eq!(store.admission(&exe(2)).await?.unwrap().state,runbook_core::catalog::AdmissionState::Queued);
        let w=begin_actor(&store,&directory,scope(1),&exe(2)).await.unwrap();
        patch(w,&human(1),&request,None).await.unwrap();
        assert_eq!(store.load(&exe(2)).await?.projection.status,ExecutionStatus::Cancelled);
        assert_eq!(store.get_run(&exe(2)).await?.unwrap().title,"Queued");
        assert_eq!(store.admission(&exe(2)).await?.unwrap().state,runbook_core::catalog::AdmissionState::Released);
        cancel(&store,work(&store,&directory).await,&human(1),None).await.unwrap();
        assert!(store.claim_due(100,60_000).await?.iter().all(|l| l.id.execution_id != exe(2) || l.id.kind != WorkKind::Activation));
        assert_eq!(store.admission(&exe(2)).await?.unwrap().state, runbook_core::catalog::AdmissionState::Released);
        let metadata = ExecutionPatchRequest { action: None, title: Some("After".into()), tags: None, fields: None };
        let w=begin_actor(&store,&directory,scope(1),&exe(2)).await.unwrap();
        assert_eq!(patch(w,&human(1),&metadata,None).await.unwrap_err().code(),"execution.closed");
        let revision=store.load(&exe(2)).await?.revision;
        let w=begin_actor(&store,&directory,scope(1),&exe(2)).await.unwrap();
        patch(w,&human(1),&request,None).await.unwrap();
        assert_eq!(store.load(&exe(2)).await?.revision,revision);
        Ok(())
    }).await;
}

#[tokio::test]
#[ignore = "requires owned PostgreSQL database"]
async fn actor_replay_storage_does_not_grow_session_snapshots() {
    use runbook_core::actor::Replay;
    with_factory(async |f| {
        let (store, directory) = started(f, TWO_TASKS, "actor-check").await;
        let w = work(&store, &directory).await;
        let sr = step_run(&w, "first");
        let claim = claim_step(
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
        )
        .await
        .unwrap();
        for n in 0..50 {
            let replay = Replay {
                key: format!("beat-{n}"),
                principal: human(1).principal,
                request_hash: [1; 32],
            };
            heartbeat(
                &store,
                &human(1),
                &exe(1),
                &sr,
                &HeartbeatRequest {
                    claim: claim.clone(),
                },
                Some(&replay),
            )
            .await
            .unwrap();
        }
        assert!(
            store.load(&exe(1)).await?.idempotency.is_empty(),
            "actor replays must not load into sessions"
        );

        let replay=Replay{key:"beat-49".into(),principal:human(1).principal,request_hash:[1;32]};
        let operation=format!("heartbeat:{sr}");
        let w=work(&store,&directory).await;
        assert_eq!(w.replayed::<Option<i64>>(&human(1),Some(&replay),&operation).await.unwrap(),Some(None));
        drop(w);
        let count:i64=f.admin.client().query_one("SELECT count(*) FROM runbook.actor_replay WHERE execution_id=$1",&[&exe(1).as_str()]).await.unwrap().get(0);
        assert_eq!(count,50);
        use runbook_core::session::{SessionStore,ExecutionSession};
        let session=store.begin(&exe(1)).await?;
        let snapshot=session.load(&exe(1)).await?;
        let future=Replay{key:"future".into(),principal:human(1).principal,request_hash:[1;32]};
        let batch=runbook_core::ExecutionCommit{execution_id:exe(1),expected_sequence:snapshot.revision,projection:snapshot.projection,events:vec![],timers:vec![],outbox:vec![],idempotency:vec![future.record("future",&exe(1),session.transaction_time_ms()+86_400_000,()).unwrap()],claims:vec![],credentials:vec![],derived_outbox:vec![]};
        assert_eq!(session.commit(batch).await.unwrap_err(),runbook_core::StoreError::InvalidBatch("store.transaction_time_mismatch"));
        let count:i64=f.admin.client().query_one("SELECT count(*) FROM runbook.actor_replay WHERE execution_id=$1",&[&exe(1).as_str()]).await.unwrap().get(0);
        assert_eq!(count,50,"future replay cannot purge valid records");

        f.admin.client().execute("UPDATE runbook.actor_replay SET at_ms=at_ms-86401000, body=jsonb_set(jsonb_set(body::jsonb,'{response,at_ms}',to_jsonb(at_ms-86401000)), '{key}', to_jsonb(prefix||(at_ms-86401000)::text))::text WHERE execution_id=$1",&[&exe(1).as_str()]).await.unwrap();
        let w=work(&store,&directory).await;
        assert_eq!(w.replayed::<Option<i64>>(&human(1),Some(&replay),&operation).await.unwrap(),None);
        drop(w);
        heartbeat(&store,&human(1),&exe(1),&sr,&HeartbeatRequest{claim},Some(&replay)).await.unwrap();
        let count:i64=f.admin.client().query_one("SELECT count(*) FROM runbook.actor_replay WHERE execution_id=$1",&[&exe(1).as_str()]).await.unwrap().get(0);
        assert_eq!(count,1,"expired rows are purged on insert");
        cancel(&store,work(&store,&directory).await,&human(1),None).await.unwrap();
        use runbook_core::{retention::RunStore,step_lookup::StepRunLookup};
        assert_eq!(store.export_execution(&exe(1)).await?.actor_replays.len(),1);
        assert!(store.step_run_execution(&scope(1).org_id,&sr).await?.is_some());
        store.delete_execution(&exe(1),"test purge").await?;
        assert_eq!(store.step_run_execution(&scope(1).org_id,&sr).await?,None);
        let count:i64=f.admin.client().query_one("SELECT count(*) FROM runbook.actor_replay WHERE execution_id=$1",&[&exe(1).as_str()]).await.unwrap().get(0);
        assert_eq!(count,0);
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires owned PostgreSQL database"]
async fn admin_release_audits_admin_and_fences_owner() {
    use runbook_core::{
        actor::Replay,
        claims::{admin_release, update_checklist},
    };
    with_factory(async |f| {
        let (store,directory)=started(f,TWO_TASKS,"actor-check").await;
        let w=work(&store,&directory).await; let sr=step_run(&w,"first");
        let owner=human(1);
        let claim=claim_step(w,&owner,&sr,&ClaimRequest{session_id:"web".into(),actor:None,take_over:false},"clm_00000000000000000000000001".parse().unwrap(),None).await.unwrap();
        assert!(admin_release(work(&store,&directory).await,&human(2),&sr,"reassign",None).await.is_err());
        assert!(admin_release(work(&store,&directory).await,&human(3),&sr,"  ",None).await.is_err());
        let replay=Replay{key:"admin-release".into(),principal:human(3).principal,request_hash:[1;32]};
        store.fail_after(runbook_core::WriteStage::Claims);
        assert!(admin_release(work(&store,&directory).await,&human(3),&sr,"reassign",Some(&replay)).await.is_err());
        assert!(!store.claim_record(&exe(1),&sr).await?.unwrap().released);
        admin_release(work(&store,&directory).await,&human(3),&sr,"reassign",Some(&replay)).await.unwrap();
        let current=store.claim_record(&exe(1),&sr).await?.unwrap();
        assert!(current.released);assert_eq!(current.generation,claim.generation.get()+1);
        let snapshot=store.load(&exe(1)).await?;
        let event=snapshot.events.iter().find(|e| matches!(&e.event,runbook_engine::Event::StepClaimReleased{step_run_id,..} if step_run_id==&sr)).unwrap();
        assert_eq!(event.actor,human(3).actor);
        assert_eq!(serde_json::to_value(&event.event).unwrap()["reason"],"reassign");
        admin_release(work(&store,&directory).await,&human(3),&sr,"reassign",Some(&replay)).await.unwrap();
        assert_eq!(store.load(&exe(1)).await?.revision,snapshot.revision);
        assert!(heartbeat(&store,&owner,&exe(1),&sr,&HeartbeatRequest{claim:claim.clone()},None).await.is_err());
        assert!(append_logs(&store,&owner,&exe(1),&sr,&LogsRequest{claim:claim.clone(),logs:vec!["old".into()]},None).await.is_err());
        assert!(update_checklist(work(&store,&directory).await,&owner,&sr,&ChecklistRequest{claim:claim.clone(),checklist:vec![]},None).await.is_err());
        let error=report_step(work(&store,&directory).await,&owner,&ReportRequest{step_run_id:sr.clone(),claim:claim.clone(),status:ReportStatus::Succeeded,verdict:None,outputs:Data::new(),summary:None,evidence:vec![],logs:vec![],cost:None},None).await.unwrap_err();
        assert_eq!(error.code(),"claim.stale_generation");
        let next=claim_step(work(&store,&directory).await,&human(2),&sr,&ClaimRequest{session_id:"web-2".into(),actor:None,take_over:false},"clm_00000000000000000000000002".parse().unwrap(),None).await.unwrap();
        assert!(next.generation.get()>current.generation);
        Ok(())
    }).await;
}
