//! Runtime bounds at the boundary: the step-run cap (budget and budget+1),
//! retry exhaustion, and foreach concurrency over a whole run.
mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, ExecutionStatus, ReportStatus, StepStatus};
use support::*;

fn capped(spec: &str, cap: u64, inputs: &[Input]) -> Trace {
    run_document_with_start(
        &format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test}}\n{spec}"
        ),
        inputs,
        Default::default(),
        StartContext {
            limits: ExecutionLimits {
                max_step_runs: Some(cap),
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

/// PRD section 6 "Step cap": checked when a step run is materialized; going
/// over fails the run with `limit.step_runs_exceeded` and keeps results.
#[test]
fn step_cap_admits_exactly_the_budget() {
    // `set` nodes are not step runs; three tasks are.
    let spec = "spec: {vars: {x: a}, steps: [{id: one, title: One}, {id: mark, set: {vars: {x: b}}}, {id: two, title: Two}, {id: three, title: Three}]}";
    let inputs = [succeeded("one"), succeeded("two"), succeeded("three")];
    let exact = capped(spec, 3, &inputs);
    assert_eq!(exact.state.status, ExecutionStatus::Succeeded);
    assert_eq!(exact.state.materialized_steps, 3);
    assert_eq!(exact.count("StepLimitExceeded"), 0);
    assert_eq!(exact.state, exact.replay());

    let over = capped(spec, 2, &inputs[..2]);
    assert_eq!(over.state.status, ExecutionStatus::Failed);
    assert_eq!(over.state.materialized_steps, 2);
    assert_eq!(over.count("StepLimitExceeded"), 1);
    assert_eq!(
        over.state.terminal_reason.as_deref(),
        Some("limit.step_runs_exceeded")
    );
    assert!(over.step("three").id.is_none());
    // Results so far are kept.
    assert_eq!(over.step("two").status, StepStatus::Succeeded);
    assert_eq!(over.state.vars["x"], "b");
    assert_eq!(over.state, over.replay());
}

/// A retried attempt reuses its step run, so it never counts against the cap.
#[test]
fn retries_do_not_consume_the_step_budget() {
    let mut failed = succeeded("a");
    if let Input::Report { status, .. } = &mut failed {
        *status = ReportStatus::Failed;
    }
    let trace = capped(
        "spec: {steps: [{id: a, title: A, retry: {max_attempts: 3}}]}",
        1,
        &[failed.clone(), failed, succeeded("a")],
    );
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state.materialized_steps, 1);
    assert_eq!(trace.step("a").attempt_history.len(), 3);
    assert_eq!(trace.state, trace.replay());
}

/// `max_attempts: 3` means three attempts: two retries, then the failure
/// propagates. The backoff timer is not due one millisecond early.
#[test]
fn retry_stops_after_exactly_max_attempts() {
    let mut trace = run_case(
        "spec: {execution: {timeout: 1h}, steps: [{id: a, title: A, retry: {max_attempts: 3, backoff: 1m}}]}",
        &[],
    );
    let fail = |trace: &mut Trace| {
        trace.append(
            Event::StepReported {
                step_run_id: trace.step("a").id.clone().unwrap(),
                report: WorkReport {
                    appended_logs: false,
                    files: vec![],
                    status: ReportStatus::Failed,
                    verdict: None,
                    outputs: Default::default(),
                    summary: None,
                    evidence: vec![],
                    logs: vec![],
                },
            },
            ActorRef::System,
            trace.state.at_ms,
        );
        trace.settle();
    };
    for attempt in 1..=2u32 {
        fail(&mut trace);
        assert_eq!(trace.count("RetryScheduled"), attempt as usize);
        let due = trace.step("a").retry_timer.as_ref().unwrap().due_at_ms;
        assert_eq!(due, trace.state.at_ms + 60_000);
        assert!(advance_time(&trace.state, due - 1).unwrap().is_empty());
        for command in advance_time(&trace.state, due).unwrap() {
            trace.commit_command(command, due);
        }
        trace.settle();
        assert_eq!(trace.step("a").status, StepStatus::Ready);
        assert_eq!(trace.step("a").attempt, attempt + 1);
    }
    fail(&mut trace);
    assert_eq!(trace.count("RetryScheduled"), 2);
    assert_eq!(trace.step("a").status, StepStatus::Failed);
    assert_eq!(trace.step("a").attempt_history.len(), 3);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state, trace.replay());
}

/// `concurrency: 2` holds for the whole run, including as iterations finish
/// out of order, and every item still runs exactly once.
#[test]
fn foreach_never_exceeds_its_concurrency() {
    let mut trace = run_case(
        "spec: {inputs: {items: {type: list, items: {type: string}, default: [a, b, c, d, e]}}, steps: [{id: each, foreach: {items: '${{ inputs.items }}', key: '${{ item }}', max_items: 5, concurrency: 2, steps: [{id: work, title: Work}]}}]}",
        &[],
    );
    let open = |trace: &Trace| {
        trace
            .state
            .steps
            .iter()
            .filter(|s| {
                s.node_id == "work" && matches!(s.status, StepStatus::Ready | StepStatus::Claimed)
            })
            .map(|s| s.step_id.clone())
            .collect::<Vec<_>>()
    };
    let mut finished = vec![];
    loop {
        let now = open(&trace);
        assert!(now.len() <= 2, "{now:?}");
        let Some(next) = now.last().cloned() else {
            break;
        };
        trace.append(
            Event::StepReported {
                step_run_id: trace.step(&next).id.clone().unwrap(),
                report: WorkReport {
                    appended_logs: false,
                    files: vec![],
                    status: ReportStatus::Succeeded,
                    verdict: None,
                    outputs: Default::default(),
                    summary: None,
                    evidence: vec![],
                    logs: vec![],
                },
            },
            ActorRef::System,
            trace.state.at_ms,
        );
        trace.settle();
        finished.push(next);
    }
    finished.sort();
    assert_eq!(
        finished,
        [
            "each[a].work",
            "each[b].work",
            "each[c].work",
            "each[d].work",
            "each[e].work"
        ]
    );
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.state, trace.replay());
}
