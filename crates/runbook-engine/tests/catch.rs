mod support;
use runbook_model::{ExecutionStatus, StepStatus};
use support::*;
fn failed(step: &str) -> Input {
    let mut input = succeeded(step);
    let Input::Report { status, .. } = &mut input else {
        unreachable!()
    };
    *status = runbook_model::ReportStatus::Failed;
    input
}
const GROUP: &str = "spec: {steps: [{id: group, steps: [{id: a, title: A}], catch: [{id: recover, title: Recover}]}]}";
#[test]
fn group_catch_handles_failure() {
    let initial = run_case(GROUP, &[]);
    assert_eq!(initial.ready, ["a"]);
    let trace = run_case(GROUP, &[failed("a"), succeeded("recover")]);
    assert_eq!(trace.step("a").status, StepStatus::Failed);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn continue_does_not_invoke_catch() {
    let trace = run_case(
        "spec: {steps: [{id: group, steps: [{id: a, title: A, on_failure: continue}, {id: b, title: B}], catch: [{id: recover, title: Recover}]}]}",
        &[failed("a")],
    );
    assert_eq!(trace.ready, ["b"]);
    assert_eq!(trace.step("recover").status, StepStatus::Pending);
    assert_eq!(trace.count("CatchActivated"), 0);
    assert_eq!(trace.step("a").status, StepStatus::Failed);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn handler_failure_propagates_to_the_nearest_outer_catch() {
    let spec = "spec: {steps: [{id: outer, steps: [{id: inner, steps: [{id: a, title: A}], catch: [{id: inner_fix, title: Inner}]}, {id: unused, title: Unused}], catch: [{id: outer_fix, title: Outer}]}]}";
    let trace = run_case(
        spec,
        &[failed("a"), failed("inner_fix"), succeeded("outer_fix")],
    );
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.count("CatchActivated"), 2);
    assert_eq!(trace.step("inner").status, StepStatus::Failed);
    assert_eq!(trace.step("inner_fix").status, StepStatus::Failed);
    assert_eq!(trace.step("outer").status, StepStatus::Succeeded);
    assert!(trace.step("unused").id.is_none());
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn caught_and_continued_failures_do_not_trigger_parallel_fail_fast() {
    let spec = "spec: {steps: [{id: both, fail_fast: true, parallel: [{id: group, steps: [{id: a, title: A}], catch: [{id: fix, title: Fix}]}, {id: b, title: B}]}]}";
    let caught = run_case(spec, &[failed("a"), succeeded("fix")]);
    assert_eq!(caught.ready, ["b"]);
    assert_eq!(caught.count("SiblingsCancelled"), 0);
    let unhandled = run_case(spec, &[failed("a"), failed("fix")]);
    assert_eq!(unhandled.step("b").status, StepStatus::Cancelled);
    assert_eq!(unhandled.state.status, ExecutionStatus::Failed);
    let continued = run_case(
        "spec: {steps: [{id: both, fail_fast: true, parallel: [{id: a, title: A, on_failure: continue}, {id: b, title: B}]}]}",
        &[failed("a"), succeeded("b")],
    );
    assert_eq!(continued.state.status, ExecutionStatus::Succeeded);
    assert_eq!(continued.step("a").status, StepStatus::Failed);
    assert_eq!(continued.count("SiblingsCancelled"), 0);
    assert_eq!(continued.state, continued.replay());
}
#[test]
fn control_continue_retains_failure_and_skips_outer_catch() {
    let spec = "spec: {steps: [{id: outer, steps: [{id: group, on_failure: continue, steps: [{id: a, title: A}]}, {id: b, title: B}], catch: [{id: fix, title: Fix}]}]}";
    let trace = run_case(spec, &[failed("a"), succeeded("b")]);
    assert_eq!(trace.step("group").status, StepStatus::Failed);
    assert!(trace.step("fix").id.is_none());
    assert_eq!(trace.count("CatchActivated"), 0);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}
#[test]
fn findings_roll_up_without_activating_error_handlers() {
    let spec = "spec: {steps: [{id: group, steps: [{id: a, title: A, check: true}], catch: [{id: fix, title: Fix}]}], outputs: {result: '${{ steps.group.verdict }}'}}";
    let mut report = succeeded("a");
    let Input::Report { verdict, .. } = &mut report else {
        unreachable!()
    };
    *verdict = Some(runbook_model::Verdict::Fail);
    let trace = run_case(spec, &[report]);
    assert_eq!(trace.count("CatchActivated"), 0);
    assert_eq!(trace.state.result, serde_json::json!("fail"));
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
}
#[test]
fn catch_activation_is_fenced_and_the_recorded_handler_stays_selected() {
    use runbook_engine::{CatchSelection, Event};
    use runbook_model::ActorRef;
    let mut trace = run_case(GROUP, &[]);
    let selection = CatchSelection {
        group_run_id: trace.step("group").id.clone().unwrap(),
        failed_run_id: trace.step("a").id.clone().unwrap(),
    };
    let before = trace.state.clone();
    trace.append(
        Event::CatchActivated {
            selection: selection.clone(),
        },
        ActorRef::System,
        0,
    );
    assert_eq!(trace.error_codes, ["group.invalid_catch"]);
    assert_eq!(trace.state, before);
    let mut trace = run_case(GROUP, &[failed("a")]);
    assert_eq!(trace.ready, ["recover"]);
    let before = trace.state.clone();
    trace.append(Event::CatchActivated { selection }, ActorRef::System, 0);
    assert_eq!(trace.error_codes, ["group.invalid_catch"]);
    assert_eq!(trace.state, before);
    trace
        .state
        .steps
        .iter_mut()
        .find(|s| s.step_id == "a")
        .unwrap()
        .status = StepStatus::Succeeded;
    assert!(runbook_engine::decide(&trace.state).unwrap().is_empty());
    assert!(trace.step("group").catch_source.is_some());
}
#[test]
fn catch_inside_foreach_stays_in_its_own_iteration() {
    let spec = "spec: {inputs: {items: {type: list, items: {type: string}, default: [east, west]}}, steps: [{id: each, foreach: {items: '${{ inputs.items }}', key: '${{ item }}', max_items: 2, concurrency: 2, steps: [{id: group, steps: [{id: a, title: A}], catch: [{id: fix, title: Fix}]}]}}]}";
    let trace = run_case(
        spec,
        &[
            failed("each[east].a"),
            succeeded("each[west].a"),
            succeeded("each[east].fix"),
        ],
    );
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.count("CatchActivated"), 1);
    assert!(trace.step("each[west].fix").id.is_none());
    assert_eq!(
        trace.step("each[east].group").catch_source,
        trace.step("each[east].a").id
    );
    assert_eq!(trace.state, trace.replay());
}
