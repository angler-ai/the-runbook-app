//! Tests for REP1: pure and portable JUnit XML report renderer.
use runbook_core::{
    actor::{ActorPrincipal, begin_actor},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    claims::{claim_step, report_step},
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, memory::MemoryDirectory,
    },
    execution_document::build_document,
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    report_junit::{render_junit, xml_escape},
    start::{StartCommand, StartStore, activate, prepare_start_commit},
    work_queue::WorkQueue,
};
use runbook_engine::ExecutionState;
use runbook_model::{
    ActorRef, ClaimRequest, Data, ExecutionDocumentView, ExecutionId, ExecutionStatus,
    ExecutionView, PagedLogRef, ReportRequest, ReportStatus, RunbookId, StepRunId, StepSection,
    StepStatus, Verdict,
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
    started_with(SPEC)
}

/// A run of `spec` (the `rates` runbook), activated with its first step ready.
fn started_with(spec: &str) -> (MemoryExecutionStore, MemoryDirectory) {
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
        spec.into(),
    ))
    .unwrap();
    let prepared = prepare(
        "rates",
        spec,
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

fn make_section(
    path: &str,
    title: &str,
    status: StepStatus,
    verdict: Option<Verdict>,
    summary: Option<String>,
    outputs: Data,
) -> StepSection {
    StepSection {
        step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
        path: path.into(),
        title: title.into(),
        status,
        verdict,
        actor: None,
        assignee: None,
        taken_over: None,
        task_actor: None,
        labels: Default::default(),
        failure_type: None,
        summary,
        outputs,
        evidence: vec![],
        approval: None,
        memory_read: vec![],
        memory_proposed: vec![],
        memory_saved: vec![],
        memory_edited: vec![],
        kind: Some("task".into()),
        completed_at_ms: None,
        amended: false,
        amendments: vec![],
        stale: false,
        call: None,
        followup: None,
        logs: PagedLogRef {
            href: "/logs".into(),
            next_cursor: None,
        },
    }
}

/// A golden test on a document built the way `crates/runbook-core/tests/execution_document.rs` builds one.
#[test]
fn golden_report_matches_exact_xml() {
    let (store, directory) = started();
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
            verdict: Some(Verdict::Pass),
            outputs: Data::from([("rate".to_string(), serde_json::json!(0.01))]),
            summary: Some("rates ok".into()),
            evidence: vec![],
            logs: vec![],
        },
        None,
    ))
    .unwrap();

    let done = state(&store, &directory);
    let mut hdr = header(&done);
    hdr.runbook_slug = Some("rates".into());
    let document = build_document(&done, hdr, logs);

    let xml = render_junit(&document);
    let expected = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<testsuites>
  <testsuite name=\"Rates\" tests=\"1\" failures=\"0\" skipped=\"0\">
    <testcase classname=\"rates\" name=\"check\">
      <system-out>rates ok
rate: 0.01</system-out>
    </testcase>
  </testsuite>
</testsuites>
";
    assert_eq!(xml, expected);
}

/// A test that a summary containing `<script>&"'` and a `\u{0001}` control char comes out escaped and stripped.
#[test]
fn summary_with_special_characters_is_escaped_and_stripped() {
    let malicious = "<script>alert('xss')</script>&\"'\u{0001}payload";
    assert_eq!(
        xml_escape(malicious),
        "&lt;script&gt;alert(&apos;xss&apos;)&lt;/script&gt;&amp;&quot;&apos;payload"
    );

    let doc = ExecutionDocumentView {
        header: ExecutionView {
            schema_revision: NonZeroU32::MIN,
            id: exe(),
            runbook_id: None,
            runbook_slug: Some("security-runbook".into()),
            project_id: None,
            created_at_ms: None,
            created_by: None,
            started_at_ms: None,
            completed_at_ms: None,
            runbook_version_id: "rbv_00000000000000000000000001".parse().unwrap(),
            runbook_version: Some(1),
            title: "Security & Test <Run>".into(),
            status: ExecutionStatus::Failed,
            phase: runbook_model::ExecutionPhase::Ordinary,
            admission: None,
            execution_deadline_at_ms: None,
            result: None,
            pending_terminal_status: None,
            terminal_reason: None,
            inputs: Data::new(),
            fields: Data::new(),
            tags: vec![],
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
        },
        sections: vec![make_section(
            "vuln_check",
            "Vulnerability Check",
            StepStatus::Failed,
            Some(Verdict::Fail),
            Some(malicious.into()),
            Data::from([("detail".into(), serde_json::json!("<tag>&123"))]),
        )],
    };

    let xml = render_junit(&doc);
    assert!(
        !xml.contains("<script>"),
        "raw script tags must not be emitted"
    );
    assert!(
        !xml.contains('\u{0001}'),
        "invalid XML control char must be stripped"
    );
    assert!(
        xml.contains("&lt;script&gt;alert(&apos;xss&apos;)&lt;/script&gt;&amp;&quot;&apos;payload")
    );
    assert!(xml.contains("Security &amp; Test &lt;Run&gt;"));
    assert!(xml.contains("detail: &lt;tag&gt;&amp;123"));
}

/// A test that `failures` and `skipped` counts match.
#[test]
fn failures_and_skipped_counts_match() {
    let doc = ExecutionDocumentView {
        header: ExecutionView {
            schema_revision: NonZeroU32::MIN,
            id: exe(),
            runbook_id: None,
            runbook_slug: Some("flow".into()),
            project_id: None,
            created_at_ms: None,
            created_by: None,
            started_at_ms: None,
            completed_at_ms: None,
            runbook_version_id: "rbv_00000000000000000000000001".parse().unwrap(),
            runbook_version: Some(1),
            title: "Flow Run".into(),
            status: ExecutionStatus::Failed,
            phase: runbook_model::ExecutionPhase::Ordinary,
            admission: None,
            execution_deadline_at_ms: None,
            result: None,
            pending_terminal_status: None,
            terminal_reason: None,
            inputs: Data::new(),
            fields: Data::new(),
            tags: vec![],
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
        },
        sections: vec![
            make_section(
                "s1",
                "Step 1",
                StepStatus::Succeeded,
                Some(Verdict::Pass),
                None,
                Data::new(),
            ),
            make_section(
                "s2",
                "Step 2",
                StepStatus::Failed,
                None,
                Some("command crashed".into()),
                Data::new(),
            ),
            make_section(
                "s3",
                "Step 3",
                StepStatus::Succeeded,
                Some(Verdict::Fail),
                Some("assertion failed".into()),
                Data::new(),
            ),
            make_section(
                "s4",
                "Step 4",
                StepStatus::Failed,
                Some(Verdict::Fail),
                None,
                Data::new(),
            ),
            make_section("s5", "Step 5", StepStatus::Skipped, None, None, Data::new()),
            make_section(
                "s6",
                "Step 6",
                StepStatus::Cancelled,
                None,
                None,
                Data::new(),
            ),
            make_section(
                "",
                "Step 7 fallback name",
                StepStatus::Ready,
                None,
                None,
                Data::new(),
            ),
        ],
    };

    let xml = render_junit(&doc);

    // Header suite attributes match exact counts
    assert!(
        xml.contains("tests=\"7\" failures=\"3\" skipped=\"2\""),
        "expected tests=7, failures=3, skipped=2: {xml}"
    );

    // Number of <testcase tags matches tests count
    let testcase_count = xml.match_indices("<testcase ").count();
    assert_eq!(testcase_count, 7);

    // Number of <failure tags matches failures count
    let failure_count = xml.match_indices("<failure ").count();
    assert_eq!(failure_count, 3);

    // Number of <skipped/> tags matches skipped count
    let skipped_count = xml.match_indices("<skipped/>").count();
    assert_eq!(skipped_count, 2);

    // Check fallback to title when path is empty
    assert!(xml.contains("name=\"Step 7 fallback name\""));

    // Check failure fallback message when summary is None
    assert!(xml.contains("<failure message=\"fail\"/>"));
}

/// A test that output order is deterministic: build twice, then compare.
#[test]
fn output_order_is_deterministic() {
    let mut outputs = Data::new();
    outputs.insert("zebra".into(), serde_json::json!("stripes"));
    outputs.insert("alpha".into(), serde_json::json!(100));
    outputs.insert("beta".into(), serde_json::json!(true));
    outputs.insert("gamma".into(), serde_json::json!([1, 2, 3]));

    let doc = ExecutionDocumentView {
        header: ExecutionView {
            schema_revision: NonZeroU32::MIN,
            id: exe(),
            runbook_id: Some("rb_00000000000000000000000001".parse().unwrap()),
            runbook_slug: None,
            project_id: None,
            created_at_ms: None,
            created_by: None,
            started_at_ms: None,
            completed_at_ms: None,
            runbook_version_id: "rbv_00000000000000000000000001".parse().unwrap(),
            runbook_version: Some(2),
            title: "Deterministic Suite".into(),
            status: ExecutionStatus::Succeeded,
            phase: runbook_model::ExecutionPhase::Ordinary,
            admission: None,
            execution_deadline_at_ms: None,
            result: None,
            pending_terminal_status: None,
            terminal_reason: None,
            inputs: Data::new(),
            fields: Data::new(),
            tags: vec![],
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
        },
        sections: vec![
            make_section(
                "step_b",
                "Step B",
                StepStatus::Succeeded,
                Some(Verdict::Pass),
                Some("first".into()),
                outputs.clone(),
            ),
            make_section(
                "step_a",
                "Step A",
                StepStatus::Succeeded,
                Some(Verdict::Pass),
                Some("second".into()),
                outputs,
            ),
        ],
    };

    let first = render_junit(&doc);
    let second = render_junit(&doc);
    assert_eq!(first, second);

    // Ensure outputs are in key order: alpha, beta, gamma, zebra
    let alpha_pos = first.find("alpha: 100").unwrap();
    let beta_pos = first.find("beta: true").unwrap();
    let gamma_pos = first.find("gamma: [1,2,3]").unwrap();
    let zebra_pos = first.find("zebra: stripes").unwrap();
    assert!(alpha_pos < beta_pos);
    assert!(beta_pos < gamma_pos);
    assert!(gamma_pos < zebra_pos);

    // Classname falls back to runbook_id when slug is None
    assert!(first.contains("classname=\"rb_00000000000000000000000001\""));
}

/// REP2: the served JUnit names people the way the page does (a known user
/// by name, anyone else by id) and, like the Markdown export, carries the
/// custom report only once the run is terminal (D34). The unnamed renderer
/// keeps its output.
#[test]
fn named_junit_names_people_and_reports_only_when_terminal() {
    use runbook_core::report_junit::render_junit_named;
    use runbook_model::UserId;
    use std::collections::BTreeMap;

    // A constant report: it could render at any time, so only D34 keeps it
    // out of a live run.
    let (store, directory) = started_with(&SPEC.replace(
        "report: 'Result: ${{ outputs.result }}'",
        "report: 'Result: healthy'",
    ));
    let live = state(&store, &directory);
    let id = check_id(&live);
    let alice: UserId = "usr_00000000000000000000000001".parse().unwrap();
    let names = BTreeMap::from([(alice.clone(), "Alice <Ops> & Co".to_string())]);

    // Live: a person on the step, no custom report yet.
    let mut document = build_document(&live, header(&live), logs);
    document.sections[0].actor = Some(ActorRef::Human {
        user_id: alice.clone(),
    });
    document.sections[0].approval = Some(runbook_model::ApprovalView {
        schema_revision: NonZeroU32::MIN,
        step_run_id: id.clone(),
        status: StepStatus::Succeeded,
        prompt: "Go?".into(),
        options: vec![],
        eligible_recipients: vec![],
        escalation_recipients: vec![],
        escalated: false,
        quorum: NonZeroU32::MIN,
        decisions: vec![runbook_model::DecisionRecord::Human {
            user_id: alice.clone(),
            membership_revision: std::num::NonZeroU64::MIN,
            option: "keep".into(),
            comment: Some("fine".into()),
        }],
    });
    let xml = render_junit_named(&live, &document, &names);
    assert!(
        xml.contains(
            "<system-out>Actor: Alice &lt;Ops&gt; &amp; Co\n\
             Decision: Alice &lt;Ops&gt; &amp; Co chose keep: fine</system-out>"
        ),
        "{xml}"
    );
    assert!(!xml.contains(alice.as_str()), "named, never by id: {xml}");
    assert!(!xml.contains("Result: healthy"), "D34: {xml}");
    let unnamed = render_junit_named(&live, &document, &BTreeMap::new());
    assert!(
        unnamed.contains(&format!("Actor: human {alice}")),
        "an unknown person falls back to the id: {unnamed}"
    );

    // Terminal: the report appears on the suite.
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
            verdict: Some(Verdict::Pass),
            outputs: Data::from([("rate".to_string(), serde_json::json!(0.01))]),
            summary: Some("rates ok".into()),
            evidence: vec![],
            logs: vec![],
        },
        None,
    ))
    .unwrap();
    let done = state(&store, &directory);
    assert_eq!(done.status, ExecutionStatus::Succeeded);
    let document = build_document(&done, header(&done), logs);
    let xml = render_junit_named(&done, &document, &names);
    assert!(
        xml.contains("<system-out>rates ok\nrate: 0.01\nActor: agent bot</system-out>"),
        "{xml}"
    );
    assert!(
        xml.contains(
            "    </testcase>\n    <system-out>Result: healthy</system-out>\n  </testsuite>"
        ),
        "{xml}"
    );
    // The unnamed renderer is unchanged: no actor lines, no report.
    let plain = render_junit(&document);
    assert!(!plain.contains("Actor:"), "{plain}");
    assert!(!plain.contains("Result: healthy"), "{plain}");
}
