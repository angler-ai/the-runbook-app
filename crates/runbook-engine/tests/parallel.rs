mod support;
use runbook_model::{ExecutionStatus, StepStatus};
use support::*;

const JOIN: &str = "spec: {steps: [{id: both, parallel: [{id: a, title: A}, {id: b, title: B}]}, {id: c, title: C}]}";
#[test]
fn join_all_waits_for_every_child() {
    let started = run_case(JOIN, &[]);
    assert_eq!(started.ready, ["a", "b"]);
    assert!(started.error_codes.is_empty());
    let trace = run_case(JOIN, &[succeeded("a")]);
    assert_eq!(trace.ready, ["b"]);
    assert_eq!(trace.step("c").status, StepStatus::Pending);
    assert_eq!(trace.state, trace.replay());
    let joined = run_case(JOIN, &[succeeded("b"), succeeded("a"), succeeded("c")]);
    assert_eq!(joined.state.status, ExecutionStatus::Succeeded);
    assert_eq!(joined.step("both").status, StepStatus::Succeeded);
    assert_eq!(joined.state, joined.replay());
}
fn failed(step: &str) -> Input {
    let mut input = succeeded(step);
    let Input::Report { status, .. } = &mut input else {
        unreachable!()
    };
    *status = runbook_model::ReportStatus::Failed;
    input
}
#[test]
fn fail_fast_cancels_open_siblings() {
    let trace = run_case(
        "spec: {steps: [{id: both, fail_fast: true, parallel: [{id: a, title: A, on_failure: fail}, {id: b, title: B}]}, {id: c, title: C}]}",
        &[failed("a")],
    );
    assert_eq!(trace.step("b").status, StepStatus::Cancelled);
    assert_eq!(trace.step("a").status, StepStatus::Failed);
    assert_eq!(trace.step("both").status, StepStatus::Failed);
    assert_eq!(trace.step("c").status, StepStatus::Pending);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert!(trace.ready.is_empty());
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn bad_findings_do_not_cancel_siblings() {
    let spec = "spec: {steps: [{id: both, fail_fast: true, parallel: [{id: a, title: A, check: true}, {id: b, title: B, check: true}]}], outputs: {result: '${{ steps.both.verdict }}'}}";
    let mut a = succeeded("a");
    let Input::Report { verdict, .. } = &mut a else {
        unreachable!()
    };
    *verdict = Some(runbook_model::Verdict::Fail);
    let waiting = run_case(spec, &[a.clone()]);
    assert_eq!(waiting.ready, ["b"]);
    assert_eq!(waiting.count("SiblingsCancelled"), 0);
    let mut b = succeeded("b");
    let Input::Report { verdict, .. } = &mut b else {
        unreachable!()
    };
    *verdict = Some(runbook_model::Verdict::Pass);
    let done = run_case(spec, &[a, b]);
    assert_eq!(done.state.status, ExecutionStatus::Succeeded);
    assert_eq!(
        done.step("both").verdict,
        Some(runbook_model::Verdict::Fail)
    );
    assert_eq!(done.state.result, serde_json::json!("fail"));
    assert_eq!(done.state, done.replay());
}
#[test]
fn join_all_without_fail_fast_waits_even_after_failure() {
    let waiting = run_case(JOIN, &[failed("a")]);
    assert_eq!(waiting.state.status, ExecutionStatus::Running);
    assert_eq!(waiting.ready, ["b"]);
    assert_eq!(waiting.count("SiblingsCancelled"), 0);
    let done = run_case(JOIN, &[failed("a"), succeeded("b")]);
    assert_eq!(done.state.status, ExecutionStatus::Failed);
    assert_eq!(done.step("both").status, StepStatus::Failed);
    assert!(done.step("c").id.is_none());
    assert_eq!(done.state, done.replay());
}
#[test]
fn nested_failure_cancels_open_descendants_only() {
    let spec = "spec:\n  steps:\n    - id: outer\n      fail_fast: true\n      parallel:\n        - id: left\n          switch:\n            cases: [{when: '${{ true }}', steps: [{id: a, title: A}, {id: after, title: Never}]}]\n        - id: right\n          parallel:\n            - {id: b, title: B}\n            - {id: c, title: C}\n        - {id: done, title: Done}\n";
    let trace = run_case(spec, &[succeeded("done"), failed("a")]);
    assert!(trace.error_codes.is_empty());
    for id in ["right", "b", "c"] {
        assert_eq!(trace.step(id).status, StepStatus::Cancelled, "{id}");
    }
    for id in ["left", "outer", "a"] {
        assert_eq!(trace.step(id).status, StepStatus::Failed, "{id}");
    }
    assert_eq!(trace.step("done").status, StepStatus::Succeeded);
    assert!(trace.step("after").id.is_none());
    assert_eq!(trace.count("SiblingsCancelled"), 1);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn nested_join_and_guards_preserve_independent_frontiers() {
    let spec = "spec:\n  steps:\n    - id: outer\n      parallel:\n        - id: left\n          switch:\n            cases: [{when: '${{ true }}', steps: [{id: a, title: A}, {id: b, title: B}]}]\n        - id: right\n          parallel: [{id: skip, title: Skip, if: '${{ false }}'}, {id: c, title: C}]\n    - {id: end, title: End}\n";
    let trace = run_case(spec, &[succeeded("a"), succeeded("c")]);
    assert_eq!(trace.ready, ["b"]);
    assert_eq!(trace.step("right").status, StepStatus::Succeeded);
    assert_eq!(trace.step("right").verdict, None);
    assert_eq!(trace.step("end").status, StepStatus::Pending);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn parallel_assignments_commit_on_distinct_sequences() {
    let trace = run_case(
        "spec: {vars: {x: old, y: old}, steps: [{id: both, parallel: [{id: a, set: {vars: {x: new}}}, {id: b, set: {vars: {y: '${{ vars.x }}'}}}]}], outputs: {result: '${{ vars.y }}'}}",
        &[],
    );
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.state.result, serde_json::json!("new"));
    assert_eq!(trace.count("DataChanged"), 2);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn cancellation_is_fenced_exactly_and_duplicate_or_late_facts_fail() {
    use runbook_engine::{Command, Event, WorkReport, decide};
    use runbook_model::{ActorRef, Data, ReportStatus};
    let mut trace = run_case(
        "spec: {steps: [{id: both, fail_fast: true, parallel: [{id: a, title: A}, {id: b, title: B}]}, {id: later, title: Later}]}",
        &[],
    );
    let report = |status| WorkReport {
        appended_logs: false,
        files: vec![],
        status,
        verdict: None,
        outputs: Data::new(),
        summary: None,
        evidence: vec![],
        logs: vec![],
    };
    trace.append(
        Event::StepReported {
            step_run_id: trace.step("a").id.clone().unwrap(),
            report: report(ReportStatus::Failed),
        },
        ActorRef::System,
        1,
    );
    let commands = decide(&trace.state).unwrap();
    let Command::CancelSiblings {
        parallel_run_id,
        step_run_ids,
    } = &commands[0]
    else {
        panic!("expected cancellation");
    };
    let valid = Event::SiblingsCancelled {
        parallel_run_id: parallel_run_id.clone(),
        step_run_ids: step_run_ids.clone(),
    };
    let state = trace.state.clone();
    trace.append(
        Event::SiblingsCancelled {
            parallel_run_id: parallel_run_id.clone(),
            step_run_ids: vec![trace.step("a").id.clone().unwrap()],
        },
        ActorRef::System,
        1,
    );
    assert_eq!(trace.error_codes, ["step.invalid_cancellation"]);
    assert_eq!(trace.state, state);
    trace.append(valid.clone(), ActorRef::System, 1);
    assert_eq!(trace.step("b").status, StepStatus::Cancelled);
    trace.append(valid, ActorRef::System, 1);
    assert_eq!(
        trace.error_codes.last().unwrap(),
        "step.invalid_cancellation"
    );
    trace.append(
        Event::StepReported {
            step_run_id: trace.step("b").id.clone().unwrap(),
            report: report(ReportStatus::Succeeded),
        },
        ActorRef::System,
        2,
    );
    assert_eq!(trace.error_codes.last().unwrap(), "step.not_reportable");
    trace.settle();
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn fail_fast_suppresses_siblings_not_yet_materialized() {
    use runbook_engine::{Event, WorkReport};
    use runbook_model::{ActorRef, Data, ReportStatus};
    let mut trace = run_case(
        "spec: {steps: [{id: both, fail_fast: true, parallel: [{id: a, title: A}, {id: b, title: B}]}]}",
        &[],
    );
    let first_ready = trace
        .events
        .iter()
        .position(|e| matches!(&e.event, Event::StepReady { step_id, .. } if step_id == "a"))
        .unwrap();
    trace.events.truncate(first_ready + 1);
    trace.state = trace.replay();
    trace.append(
        Event::StepReported {
            step_run_id: trace.step("a").id.clone().unwrap(),
            report: WorkReport {
                appended_logs: false,
                files: vec![],
                status: ReportStatus::Failed,
                verdict: None,
                outputs: Data::new(),
                summary: None,
                evidence: vec![],
                logs: vec![],
            },
        },
        ActorRef::System,
        1,
    );
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert!(trace.step("b").id.is_none());
    assert_eq!(trace.count("SiblingsCancelled"), 0);
    assert_eq!(trace.state, trace.replay());
}
