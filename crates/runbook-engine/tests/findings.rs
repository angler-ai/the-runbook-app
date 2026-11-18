mod support;
use runbook_model::{Data, ReportStatus, Verdict};
use support::*;
const CHECK: &str = "spec:\n  steps:\n    - id: a\n      title: Check\n      check: true\n      outputs: {code: {type: integer, required: true}}\n    - {id: b, title: Next}\n";
fn report(verdict: Option<Verdict>) -> Input {
    let mut input = succeeded("a");
    let Input::Report {
        verdict: v,
        outputs,
        ..
    } = &mut input
    else {
        unreachable!()
    };
    *v = verdict;
    *outputs = [("code".into(), serde_json::json!(503))].into();
    input
}
#[test]
fn bad_news_advances_without_error_handling() {
    let trace = run_case(CHECK, &[report(Some(Verdict::Fail))]);
    assert_eq!(trace.ready, ["b"]);
    assert_eq!(trace.step("a").verdict, Some(Verdict::Fail));
    assert_eq!(trace.count("StepReported"), 1);
    assert!(trace.error_codes.is_empty());
}
#[test]
fn successful_check_needs_verdict() {
    let trace = run_case(CHECK, &[report(None)]);
    assert_eq!(trace.error_codes, ["step.verdict_required"]);
    assert_eq!(trace.count("StepReported"), 0);
    assert_eq!(trace.ready, ["a"]);
}
fn view(verdict: Option<Verdict>) -> runbook_model::StepView {
    runbook_model::StepView {
        schema_revision: 1.try_into().unwrap(),
        id: "sr_00000000000000000000000001".parse().unwrap(),
        path: "a".into(),
        status: runbook_model::StepStatus::Succeeded,
        verdict,
        outputs: Data::new(),
        summary: None,
        actor: None,
        kind: None,
        completed_at_ms: None,
        claim: None,
        attempt: 1.try_into().unwrap(),
        satisfied: false,
        exhausted: false,
        checklist: vec![],
        amended: false,
        amendments: vec![],
        stale: false,
        eligible_recipients: vec![],
        call: None,
        wait: None,
        actor_kind: None,
        assignees: vec![],
        branch: None,
        iterations: None,
        skip_reason: None,
        followup: None,
    }
}
#[test]
fn no_finding_is_not_pass() {
    use runbook_engine::rollup_verdict;
    assert_eq!(rollup_verdict(&[]), None);
    assert_eq!(rollup_verdict(&[view(None), view(None)]), None);
    assert_eq!(
        rollup_verdict(&[view(None), view(Some(Verdict::Pass))]),
        Some(Verdict::Pass)
    );
    assert_eq!(
        rollup_verdict(&[view(Some(Verdict::Warn)), view(Some(Verdict::Pass))]),
        Some(Verdict::Warn)
    );
    assert_eq!(
        rollup_verdict(&[
            view(Some(Verdict::Pass)),
            view(Some(Verdict::Fail)),
            view(Some(Verdict::Warn))
        ]),
        Some(Verdict::Fail)
    );
}
fn request(trace: &Trace, status: ReportStatus) -> runbook_model::ReportRequest {
    runbook_model::ReportRequest {
        step_run_id: trace.step("a").id.clone().unwrap(),
        claim: runbook_model::ClaimRef {
            id: "clm_00000000000000000000000001".parse().unwrap(),
            generation: 1.try_into().unwrap(),
        },
        status,
        verdict: Some(Verdict::Pass),
        outputs: Data::new(),
        summary: None,
        evidence: vec![],
        logs: vec![],
    }
}
#[test]
fn successful_outputs_are_closed_and_typed() {
    let trace = run_case(CHECK, &[]);
    let node = &trace.state.compiled().ir().steps[0];
    let mut req = request(&trace, ReportStatus::Succeeded);
    for (outputs, code, path) in [
        (serde_json::json!({}), "data.required", "outputs.code"),
        (
            serde_json::json!({"code":"503"}),
            "data.type_error",
            "outputs.code",
        ),
        (
            serde_json::json!({"code":503,"extra":true}),
            "data.unknown_field",
            "outputs.extra",
        ),
    ] {
        req.outputs = serde_json::from_value(outputs).unwrap();
        let err = runbook_engine::validate_report(node, &req, trace.step("a")).unwrap_err();
        assert_eq!(err.code(), "step.invalid_outputs");
        assert_eq!(err.errors()[0].code, code);
        assert_eq!(err.errors()[0].path, path);
    }
    req.outputs = [("code".into(), serde_json::json!(503))].into();
    assert!(runbook_engine::validate_report(node, &req, trace.step("a")).is_ok());
}
#[test]
fn inability_reports_do_not_require_success_artifacts() {
    let trace = run_case(
        "spec:\n  steps:\n    - id: a\n      title: Check\n      check: true\n      checklist: [Done]\n      evidence: {required: [link, log]}\n      outputs: {code: {type: integer, required: true}}",
        &[],
    );
    let node = &trace.state.compiled().ir().steps[0];
    for status in [ReportStatus::Failed, ReportStatus::Blocked] {
        let mut req = request(&trace, status);
        let checked = runbook_engine::validate_report(node, &req, trace.step("a")).unwrap();
        assert_eq!(checked.work().verdict, None);
        req.outputs = [("code".into(), serde_json::json!("invalid"))].into();
        assert_eq!(
            runbook_engine::validate_report(node, &req, trace.step("a"))
                .unwrap_err()
                .code(),
            "step.invalid_outputs"
        );
    }
}
#[test]
fn successful_checklist_requires_persisted_ordered_ticks() {
    use runbook_engine::{Event, validate_report};
    use runbook_model::{ActorRef, ChecklistTick};
    let mut trace = run_case(
        "spec: {steps: [{id: a, title: Task, checklist: [One, Two]}]}",
        &[],
    );
    let req = request(&trace, ReportStatus::Succeeded);
    assert_eq!(
        validate_report(&trace.state.compiled().ir().steps[0], &req, trace.step("a"))
            .unwrap_err()
            .code(),
        "step.checklist_incomplete"
    );
    let ticks = ["One", "Two"]
        .map(|title| ChecklistTick {
            title: title.into(),
            completed: true,
        })
        .to_vec();
    trace.append(
        Event::ChecklistUpdated {
            step_run_id: req.step_run_id.clone(),
            ticks: ticks.clone(),
        },
        ActorRef::System,
        1,
    );
    assert!(trace.error_codes.is_empty());
    let accepted =
        validate_report(&trace.state.compiled().ir().steps[0], &req, trace.step("a")).unwrap();
    trace.append(accepted.into_event(), ActorRef::System, 2);
    trace.settle();
    assert_eq!(trace.step("a").checklist, ticks);
    assert_eq!(trace.state, trace.replay());
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
}
#[test]
fn required_and_accepted_evidence_kinds_are_enforced() {
    let trace = run_case(
        "spec: {steps: [{id: a, title: Task, evidence: {required: [link, log], accepted: [link, log]}}]}",
        &[],
    );
    let node = &trace.state.compiled().ir().steps[0];
    let mut req = request(&trace, ReportStatus::Succeeded);
    assert_eq!(
        runbook_engine::validate_report(node, &req, trace.step("a"))
            .unwrap_err()
            .code(),
        "step.evidence_required"
    );
    req.evidence = vec!["https://example.test/result".into()];
    req.logs = vec![" ".into()];
    assert_eq!(
        runbook_engine::validate_report(node, &req, trace.step("a"))
            .unwrap_err()
            .code(),
        "step.evidence_required"
    );
    req.logs = vec!["checked endpoint".into()];
    assert!(runbook_engine::validate_report(node, &req, trace.step("a")).is_ok());
    let links_only = run_case(
        "spec: {steps: [{id: a, title: Task, evidence: {accepted: [link]}}]}",
        &[],
    );
    assert_eq!(
        runbook_engine::validate_report(
            &links_only.state.compiled().ir().steps[0],
            &req,
            links_only.step("a")
        )
        .unwrap_err()
        .code(),
        "step.evidence_not_accepted"
    );
}
#[test]
fn recorded_outputs_preserve_exact_json_values() {
    use runbook_engine::{Event, WorkReport};
    let outputs = [
        ("decimal".into(), serde_json::from_str("1.100").unwrap()),
        (
            "literal".into(),
            serde_json::Value::Object(
                [(
                    "$serde_json::private::Number".into(),
                    serde_json::json!("7"),
                )]
                .into_iter()
                .collect(),
            ),
        ),
    ]
    .into();
    let event = Event::StepReported {
        step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
        report: WorkReport {
            appended_logs: false,
            files: vec![],
            status: ReportStatus::Succeeded,
            verdict: None,
            outputs,
            summary: None,
            evidence: vec![],
            logs: vec![],
        },
    };
    let bytes = serde_json::to_vec(&event).unwrap();
    assert_eq!(serde_json::from_slice::<Event>(&bytes).unwrap(), event);
}
#[test]
fn non_checks_and_inability_have_no_finding() {
    let mut input = succeeded("a");
    let Input::Report { verdict, .. } = &mut input else {
        unreachable!()
    };
    *verdict = Some(Verdict::Fail);
    let trace = run_case("spec: {steps: [{id: a, title: Task}]}", &[input]);
    assert_eq!(trace.step("a").verdict, None);
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
}
#[test]
fn skipping_requires_opt_in_and_reason() {
    let no_skip = run_case("spec: {steps: [{id: a, title: Task}]}", &[]);
    let mut req = request(&no_skip, ReportStatus::Skipped);
    assert_eq!(
        runbook_engine::validate_report(
            &no_skip.state.compiled().ir().steps[0],
            &req,
            no_skip.step("a")
        )
        .unwrap_err()
        .code(),
        "step.not_skippable"
    );
    let skip = run_case(
        "spec: {steps: [{id: a, title: Task, skippable: true, check: true, checklist: [Done], evidence: {required: [link]}, outputs: {answer: {type: integer, required: true}}}]}",
        &[],
    );
    let node = &skip.state.compiled().ir().steps[0];
    assert_eq!(
        runbook_engine::validate_report(node, &req, skip.step("a"))
            .unwrap_err()
            .code(),
        "step.skip_reason_required"
    );
    req.summary = Some("Not applicable in this region".into());
    assert_eq!(
        runbook_engine::validate_report(node, &req, skip.step("a"))
            .unwrap()
            .work()
            .verdict,
        None
    );
}
#[test]
fn rejected_deep_output_is_not_cloned() {
    let trace = run_case(CHECK, &[]);
    let mut req = request(&trace, ReportStatus::Succeeded);
    let mut nested = serde_json::json!(0);
    for _ in 0..20_000 {
        nested = serde_json::Value::Array(vec![nested]);
    }
    req.outputs.insert("code".into(), nested);
    let result = runbook_engine::validate_report(
        &trace.state.compiled().ir().steps[0],
        &req,
        trace.step("a"),
    );
    // Test-owned invalid data must also be dropped iteratively.
    let mut value = req.outputs.remove("code").unwrap();
    while let serde_json::Value::Array(mut values) = value {
        value = values.pop().unwrap();
    }
    assert_eq!(result.unwrap_err().errors()[0].code, "data.depth_limit");
}
#[test]
fn checklist_order_is_checked_before_recording() {
    use runbook_engine::Event;
    use runbook_model::{ActorRef, ChecklistTick};
    let mut trace = run_case(
        "spec: {steps: [{id: a, title: Task, checklist: [One, Two]}]}",
        &[],
    );
    trace.append(
        Event::ChecklistUpdated {
            step_run_id: trace.step("a").id.clone().unwrap(),
            ticks: ["Two", "One"]
                .map(|title| ChecklistTick {
                    title: title.into(),
                    completed: true,
                })
                .into(),
        },
        ActorRef::System,
        1,
    );
    assert_eq!(trace.error_codes, ["step.invalid_checklist"]);
    assert_eq!(trace.count("ChecklistUpdated"), 0);
    assert!(trace.step("a").checklist.iter().all(|t| !t.completed));
}
