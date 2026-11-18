//! LANG9: failures have a type (`timeout`, `failed`, `call_failed`) derived
//! from what the engine records. `retry.on` retries only those types;
//! `catch_on` makes a group's catch handle only those types, and other
//! failures propagate as if there were no catch. Without either, behavior is
//! unchanged.
mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, ExecutionStatus, ReportStatus, StepStatus};
use runbook_spec::ir::FailureType;
use support::*;

fn agent_report(step: &str, status: ReportStatus) -> Input {
    let mut input = succeeded(step);
    let Input::Report {
        status: recorded,
        actor,
        ..
    } = &mut input
    else {
        unreachable!()
    };
    *recorded = status;
    *actor = ActorRef::Agent {
        principal_id: "service-1".into(),
        session_id: "session-1".into(),
        name: "test".into(),
        model: None,
    };
    input
}

fn tick(trace: &mut Trace, at_ms: i64) {
    for _ in 0..20 {
        let commands = advance_time(&trace.state, at_ms).unwrap();
        if commands.is_empty() {
            break;
        }
        for command in commands {
            trace.commit_command(command, at_ms);
        }
    }
    trace.settle();
}

#[test]
fn retry_on_timeout_retries_a_timeout() {
    let mut trace = run_case(
        "spec: {steps: [{id: work, title: Work, timeout: 10m, retry: {max_attempts: 2, on: [timeout]}}]}",
        &[],
    );
    tick(&mut trace, 600_000);
    assert_eq!(trace.count("RetryScheduled"), 1);
    assert_eq!(trace.step("work").attempt, 2);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn retry_on_timeout_does_not_retry_a_reported_failure() {
    let trace = run_case(
        "spec: {steps: [{id: work, title: Work, actor: agent, retry: {max_attempts: 3, on: [timeout]}}]}",
        &[agent_report("work", ReportStatus::Failed)],
    );
    assert_eq!(trace.count("RetryScheduled"), 0);
    assert_eq!(trace.step("work").status, StepStatus::Failed);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(
        failure_type(&trace.state, trace.step("work")).unwrap(),
        FailureType::Failed
    );
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn retry_without_on_still_retries_any_failure() {
    let trace = run_case(
        "spec: {steps: [{id: work, title: Work, actor: agent, retry: {max_attempts: 2}}]}",
        &[agent_report("work", ReportStatus::Failed)],
    );
    assert_eq!(trace.count("RetryScheduled"), 1);
}

#[test]
fn defaults_retry_on_applies_to_every_task() {
    let trace = run_case(
        "spec: {defaults: {retry: {max_attempts: 3, on: [timeout]}}, steps: [{id: work, title: Work, actor: agent}]}",
        &[agent_report("work", ReportStatus::Failed)],
    );
    assert_eq!(trace.count("RetryScheduled"), 0);
}

#[test]
fn catch_on_handles_only_its_types() {
    let spec = "spec: {steps: [{id: guarded, steps: [{id: work, title: Work, actor: agent, timeout: 10m}], catch: [{id: recover, title: Recover}], catch_on: [timeout]}]}";

    // A reported failure isn't a timeout: the catch doesn't run and the run fails.
    let failed = run_case(spec, &[agent_report("work", ReportStatus::Failed)]);
    assert_eq!(failed.count("CatchActivated"), 0);
    assert_eq!(failed.state.status, ExecutionStatus::Failed);
    assert_eq!(failed.state, failed.replay());

    // A timeout is: the catch runs.
    let mut timed_out = run_case(spec, &[]);
    tick(&mut timed_out, 600_000);
    assert_eq!(timed_out.count("CatchActivated"), 1);
    assert_eq!(timed_out.ready, ["recover"]);
    assert_eq!(
        failure_type(&timed_out.state, timed_out.step("work")).unwrap(),
        FailureType::Timeout
    );
    assert_eq!(timed_out.state, timed_out.replay());
}

#[test]
fn an_outer_catch_handles_what_an_inner_typed_catch_does_not() {
    let spec = "spec: {steps: [{id: outer, steps: [{id: inner, steps: [{id: work, title: Work, actor: agent}], catch: [{id: on_timeout, title: On timeout}], catch_on: [timeout]}], catch: [{id: on_anything, title: On anything}]}]}";
    let trace = run_case(spec, &[agent_report("work", ReportStatus::Failed)]);
    assert_eq!(trace.count("CatchActivated"), 1);
    assert_eq!(trace.ready, ["on_anything"]);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn a_container_failure_takes_its_failed_steps_type() {
    let spec = "spec: {steps: [{id: guarded, steps: [{id: both, parallel: [{id: a, title: A, actor: agent, timeout: 10m}, {id: b, title: B, actor: agent, timeout: 1h}]}], catch: [{id: recover, title: Recover}], catch_on: [timeout]}]}";
    // The parallel fails once both branches end: `b` succeeds, `a` times out.
    let mut trace = run_case(spec, &[agent_report("b", ReportStatus::Succeeded)]);
    tick(&mut trace, 600_000);
    assert_eq!(trace.count("CatchActivated"), 1, "{:?}", trace.error_codes);
    assert_eq!(
        failure_type(&trace.state, trace.step("both")).unwrap(),
        FailureType::Timeout
    );
    assert_eq!(trace.state, trace.replay());
}
