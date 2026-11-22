//! B8d `live_and_terminal_exports_share_document` (core part): the live JSON
//! document and its Markdown export come from one view; untrusted text is
//! escaped; the custom report renders only once the run is terminal.
use runbook_core::{
    actor::{ActorPrincipal, begin_actor},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    claims::{claim_step, report_step},
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, memory::MemoryDirectory,
    },
    execution_document::{build_document, escape, render_markdown},
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, activate, prepare_start_commit},
    work_queue::WorkQueue,
};
use runbook_engine::ExecutionState;
use runbook_model::{
    ActorRef, ClaimRequest, Data, ExecutionId, ExecutionStatus, ExecutionView, ReportRequest,
    ReportStatus, RunbookId, StepRunId, Verdict,
};
use std::{
    future::Future,
    num::NonZeroU32,
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
const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: rates, title: Rates}\n\
    spec:\n  outputs: {result: healthy}\n  report: 'Result: ${{ outputs.result }}'\n  \
    steps:\n    - id: check\n      title: Check rates\n      \
    outputs: {rate: {type: number, required: true}}\n";

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

fn started() -> (MemoryExecutionStore, MemoryDirectory) {
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
    let store = MemoryExecutionStore::new(scope(), projection, 1_000);
    let directory = MemoryDirectory::default();
    ready(directory.commit(DirectoryCommit {
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
        ],
    }))
    .unwrap();
    let rb: RunbookId = "rb_00000000000000000000000001".parse().unwrap();
    ready(store.create_runbook(
        RunbookRecord {
            runbook_id: rb.clone(),
            slug: "rates".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        SPEC.into(),
    ))
    .unwrap();
    let prepared = prepare(
        "rates",
        SPEC,
        &Default::default(),
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
    ready(activate(&store, &directory, scope(), &activation)).unwrap();
    (store, directory)
}

fn state(store: &MemoryExecutionStore, directory: &MemoryDirectory) -> ExecutionState {
    ready(begin_actor(store, directory, scope(), &exe()))
        .unwrap()
        .runtime
        .state
}
fn header(state: &ExecutionState) -> ExecutionView {
    ExecutionView {
        schema_revision: NonZeroU32::MIN,
        id: state.id.clone(),
        runbook_id: None,
        runbook_slug: None,
        project_id: None,
        created_at_ms: None,
        created_by: None,
        started_at_ms: state.started_at_ms,
        completed_at_ms: None,
        runbook_version_id: "rbv_00000000000000000000000001".parse().unwrap(),
        runbook_version: Some(1),
        title: state.title.clone(),
        status: state.status,
        phase: runbook_model::ExecutionPhase::Ordinary,
        admission: None,
        execution_deadline_at_ms: state.execution_deadline_at_ms,
        result: None,
        pending_terminal_status: None,
        terminal_reason: None,
        inputs: state.inputs.clone(),
        fields: state.fields.clone(),
        tags: state.tags.clone(),
        steps: vec![],
        amended: false,
        amendments: vec![],
        stale_step_ids: vec![],
        rerun_of: None,
        resume_brief: None,
        notifications_not_delivered: vec![],
        superseded_by: None,
        parent: None,
        progress: None,
        cost: None,
    }
}
fn logs(id: &StepRunId) -> String {
    format!("/v1/step-runs/{id}/logs")
}
fn check_id(state: &ExecutionState) -> StepRunId {
    state
        .steps
        .iter()
        .find(|s| s.step_id == "check")
        .and_then(|s| s.id.clone())
        .expect("check is materialized")
}

#[test]
fn live_and_terminal_exports_share_document() {
    let (store, directory) = started();

    // Live: one ready task, no custom report yet, the pending header instead.
    let live = state(&store, &directory);
    let id = check_id(&live);
    let document = build_document(&live, header(&live), logs);
    assert_eq!(document.sections.len(), 1);
    assert_eq!(document.sections[0].title, "Check rates");
    assert_eq!(document.sections[0].path, "check");
    assert_eq!(document.sections[0].logs.href, logs(&id));
    let markdown = render_markdown(&live, &document);
    assert!(markdown.contains("in progress"), "{markdown}");
    assert!(!markdown.contains("## Report"), "{markdown}");

    // An agent claims and reports, with markup in its summary.
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let claim = ready(claim_step(
        w,
        &agent(),
        &id,
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
    let summary = "**bold** [x](javascript:alert(1)) <script>\n# heading";
    ready(report_step(
        w,
        &agent(),
        &ReportRequest {
            step_run_id: id.clone(),
            claim,
            status: ReportStatus::Succeeded,
            verdict: Some(Verdict::Pass),
            outputs: Data::from([("rate".to_string(), serde_json::json!(0.01))]),
            summary: Some(summary.into()),
            evidence: vec![],
            logs: vec![],
            cost: None,
        },
        None,
    ))
    .unwrap();

    // Terminal: JSON keeps raw data; Markdown escapes it and adds the report.
    let done = state(&store, &directory);
    assert_eq!(done.status, ExecutionStatus::Succeeded);
    let document = build_document(&done, header(&done), logs);
    let section = &document.sections[0];
    assert_eq!(section.status, runbook_model::StepStatus::Succeeded);
    let step = done.steps.iter().find(|s| s.step_id == "check").unwrap();
    assert_eq!(
        section.verdict, step.verdict,
        "the document mirrors the run"
    );
    assert_eq!(
        section.summary.as_deref(),
        Some(summary),
        "JSON keeps raw text"
    );
    assert_eq!(section.outputs["rate"], serde_json::json!(0.01));
    assert!(matches!(
        &section.actor,
        Some(ActorRef::Agent { name, .. }) if name == "bot"
    ));
    let markdown = render_markdown(&done, &document);
    assert!(
        markdown.contains("## Report\n\nResult: healthy"),
        "{markdown}"
    );
    assert!(!markdown.contains("in progress"), "{markdown}");
    assert!(markdown.contains("- Actor: agent bot"), "{markdown}");
    assert!(markdown.contains("```json"), "{markdown}");
    assert!(markdown.contains("\"rate\""), "{markdown}");
    assert!(markdown.contains("\\*\\*bold\\*\\*"), "{markdown}");
    assert!(!markdown.contains("](javascript"), "{markdown}");
    assert!(!markdown.contains("<script>"), "{markdown}");
    assert!(
        !markdown.contains("\n# heading"),
        "a summary cannot start a heading"
    );
    // Markdown and JSON agree on every section.
    for section in &document.sections {
        assert!(markdown.contains(&escape(&section.title)));
        assert!(markdown.contains(&section.logs.href));
    }
}

#[test]
fn escape_neutralizes_markdown_and_html() {
    assert_eq!(escape("a*b_c"), "a\\*b\\_c");
    assert_eq!(escape("[x](y)"), "\\[x\\]\\(y\\)");
    assert_eq!(escape("<b>&"), "&lt;b&gt;&amp;".replace("&gt;", "\\>"));
    assert_eq!(escape("ok\u{7}"), "ok");
}

#[test]
fn document_carries_amendment_markers() {
    let (store, directory) = started();
    // Add an admin user for the override.
    let revision = ready(directory.snapshot(&scope().org_id)).unwrap().revision;
    ready(directory.commit(runbook_core::directory::DirectoryCommit {
        org_id: scope().org_id,
        expected_revision: revision,
        mutations: vec![
            DirectoryMutation::CreateUser(runbook_core::directory::UserRecord {
                id: "usr_00000000000000000000000009".parse().unwrap(),
                name: "admin".into(),
                active: true,
                role: runbook_core::directory::OrgRole::Admin,
            }),
            DirectoryMutation::GrantProjectRole {
                project_id: scope().project_id,
                user_id: "usr_00000000000000000000000009".parse().unwrap(),
                role: runbook_model::ProjectRole::Admin,
            },
        ],
    }))
    .unwrap();
    let live = state(&store, &directory);
    let id = check_id(&live);
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    let claim = ready(claim_step(
        w,
        &agent(),
        &id,
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
            step_run_id: id.clone(),
            claim,
            status: ReportStatus::Succeeded,
            verdict: Some(runbook_model::Verdict::Pass),
            outputs: Data::from([("rate".to_string(), serde_json::json!(0.4))]),
            summary: Some("ok".into()),
            evidence: vec![],
            logs: vec![],
            cost: None,
        },
        None,
    ))
    .unwrap();
    let admin = ActorPrincipal {
        principal: "user:admin".into(),
        actor: ActorRef::Human {
            user_id: "usr_00000000000000000000000009".parse().unwrap(),
        },
        credential_ref: "pat-admin".into(),
    };
    let w = ready(begin_actor(&store, &directory, scope(), &exe())).unwrap();
    ready(runbook_core::amendments::override_step(
        w,
        &admin,
        &id,
        &runbook_model::OverrideRequest {
            to: runbook_model::StepRecord {
                status: runbook_model::StepStatus::Succeeded,
                verdict: Some(runbook_model::Verdict::Fail),
                outputs: Data::from([("rate".to_string(), serde_json::json!(2.5))]),
                summary: Some("corrected".into()),
            },
            reason: "wrong rate".into(),
        },
        None,
    ))
    .unwrap();
    let amended_state = state(&store, &directory);
    let document = build_document(&amended_state, header(&amended_state), logs);
    assert!(document.header.amended);
    assert_eq!(document.header.amendments.len(), 1);
    assert_eq!(
        document.header.amendments[0].to.outputs["rate"],
        serde_json::json!(2.5)
    );
    let section = &document.sections[0];
    assert!(section.amended);
    assert_eq!(section.amendments.len(), 1);
    assert_eq!(section.outputs["rate"], serde_json::json!(2.5));
    let brief = runbook_core::reads::resume_brief(&amended_state, None);
    assert!(brief.completed_steps[0].amended);
}

/// The Markdown export names people the way the page does: a known user by
/// name (escaped), an unknown one by id, in the actor line and in approval
/// decisions (walkthrough finding: the export listed raw `usr_` ids).
#[test]
fn markdown_export_names_people() {
    use runbook_core::execution_document::render_markdown_named;
    use runbook_model::{ApprovalView, DecisionRecord, StepStatus, UserId};
    use std::{collections::BTreeMap, num::NonZeroU64};

    let (store, directory) = started();
    let live = state(&store, &directory);
    let id = check_id(&live);
    let mut document = build_document(&live, header(&live), logs);
    let alice: UserId = "usr_00000000000000000000000001".parse().unwrap();
    let ghost: UserId = "usr_00000000000000000000000009".parse().unwrap();
    document.sections[0].actor = Some(ActorRef::Human {
        user_id: alice.clone(),
    });
    document.sections[0].approval = Some(ApprovalView {
        schema_revision: NonZeroU32::MIN,
        step_run_id: id,
        status: StepStatus::Succeeded,
        prompt: "Go?".into(),
        options: vec![],
        eligible_recipients: vec![],
        escalation_recipients: vec![],
        escalated: false,
        quorum: NonZeroU32::MIN,
        decision_lease: None,
        agent_notes: vec![],
        decisions: vec![
            DecisionRecord::Human {
                user_id: alice.clone(),
                membership_revision: NonZeroU64::MIN,
                option: "keep".into(),
                comment: None,
            },
            DecisionRecord::Human {
                user_id: ghost.clone(),
                membership_revision: NonZeroU64::MIN,
                option: "keep".into(),
                comment: None,
            },
        ],
    });
    let names = BTreeMap::from([(alice.clone(), "Alice *Ops*".to_string())]);
    let markdown = render_markdown_named(&live, &document, &names);
    assert!(
        markdown.contains("- Actor: Alice \\*Ops\\*\n"),
        "{markdown}"
    );
    assert!(
        markdown.contains("- Alice \\*Ops\\* chose keep"),
        "{markdown}"
    );
    assert!(
        !markdown.contains(alice.as_str()),
        "a known person is never shown by id: {markdown}"
    );
    assert!(
        markdown.contains(&format!("- {} chose keep", escape(ghost.as_str()))),
        "an unknown person falls back to the id: {markdown}"
    );
    // The unnamed entry point keeps its old output.
    assert!(
        render_markdown(&live, &document).contains(&format!("human {}", escape(alice.as_str())))
    );
}

/// R1-D2: the Markdown report shows a runner agent's decision with its
/// evidence, the agent reviewing an open approval, and its abstentions.
#[test]
fn markdown_shows_agent_decisions_leases_and_abstentions() {
    use runbook_model::{
        ApprovalAgentNote, ApprovalView, DecisionLeaseView, DecisionRecord, StepStatus,
    };

    let (store, directory) = started();
    let live = state(&store, &directory);
    let id = check_id(&live);
    let mut document = build_document(&live, header(&live), logs);
    let lease: runbook_model::ClaimId = "clm_01JABCDEFGHJKMNPQRSTVWXYZ0".parse().unwrap();
    let approval = ApprovalView {
        schema_revision: NonZeroU32::MIN,
        step_run_id: id,
        status: StepStatus::Waiting,
        prompt: "Ship?".into(),
        options: vec![],
        eligible_recipients: vec![],
        escalation_recipients: vec![],
        escalated: false,
        quorum: NonZeroU32::MIN,
        decision_lease: Some(DecisionLeaseView {
            lease_id: lease.clone(),
            agent_name: "release-reviewer".into(),
        }),
        agent_notes: vec![ApprovalAgentNote {
            lease_id: lease.clone(),
            principal: "runner:rin_1".into(),
            agent_name: "release-reviewer".into(),
            model: Some("claude-sonnet".into()),
            reason: "No canary data yet".into(),
            at_ms: 1,
        }],
        decisions: vec![],
    };
    document.sections[0].approval = Some(approval.clone());
    let open = render_markdown(&live, &document);
    assert!(
        open.contains("- Agent release\\-reviewer is reviewing; people can still decide\n"),
        "{open}"
    );
    assert!(
        open.contains(
            "- Agent release\\-reviewer (claude\\-sonnet) abstained: No canary data yet\n"
        ),
        "{open}"
    );

    document.sections[0].approval = Some(ApprovalView {
        status: StepStatus::Succeeded,
        decision_lease: None,
        agent_notes: vec![],
        decisions: vec![DecisionRecord::Agent {
            principal: Some("runner:rin_1".into()),
            evidence: vec!["[call 2]".into()],
            evidence_source: Some(runbook_model::AgentEvidenceSource::RunnerReported),
            agent_name: "release-reviewer".into(),
            model: Some("claude-sonnet".into()),
            session_id: lease.to_string(),
            option: "ship".into(),
            comment: Some("Error rate 0.1% [call 2]".into()),
        }],
        ..approval
    });
    let decided = render_markdown(&live, &document);
    assert!(
        decided.contains(
            "- Agent release\\-reviewer (claude\\-sonnet) chose ship: Error rate 0\\.1% \\[call 2\\] (runner-reported evidence: \\[call 2\\])\n"
        ),
        "{decided}"
    );
    assert!(!decided.contains("is reviewing"), "{decided}");
}

/// LANG7: the Markdown report shows who took over an agent task and a
/// strict assignee (with its routing error).
#[test]
fn markdown_shows_take_over_and_strict_assignee() {
    use runbook_core::execution_document::render_markdown_named;
    use runbook_model::{ActorKind, StepAssigneeView, TakenOverView, UserId};
    use std::collections::BTreeMap;

    let (store, directory) = started();
    let live = state(&store, &directory);
    let mut document = build_document(&live, header(&live), logs);
    assert_eq!(document.sections[0].taken_over, None);
    assert_eq!(document.sections[0].assignee, None);
    let alice: UserId = "usr_00000000000000000000000001".parse().unwrap();
    document.sections[0].taken_over = Some(TakenOverView {
        by: ActorRef::Human {
            user_id: alice.clone(),
        },
        at_ms: 5,
        previous_actor_kind: ActorKind::Agent,
    });
    document.sections[0].assignee = Some(StepAssigneeView {
        to: Some("team:oncall".into()),
        strict: true,
        routing_error: Some("handoff.no_recipient".into()),
        recipients: vec![],
    });
    let names = BTreeMap::from([(alice, "Alice".to_string())]);
    let markdown = render_markdown_named(&live, &document, &names);
    assert!(
        markdown.contains("- Taken over by Alice (from agent)\n"),
        "{markdown}"
    );
    assert!(
        markdown.contains("- Assignee (strict): team:oncall (handoff\\.no\\_recipient)\n"),
        "{markdown}"
    );
    // A guidance assignee adds no line.
    document.sections[0].assignee.as_mut().unwrap().strict = false;
    let markdown = render_markdown_named(&live, &document, &names);
    assert!(!markdown.contains("Assignee"), "{markdown}");
}
